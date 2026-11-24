use super::*;
use std::collections::VecDeque;

// ── Randomized convergence over three replicas ──────────────────────

/// xorshift64*: enough randomness for a scripted network, no dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> Option<T> {
        (!items.is_empty()).then(|| items[self.below(items.len())])
    }

    /// A node id from the generator, so a seed names the same ids every run.
    fn node_id(&mut self) -> NodeId {
        let (a, b) = (self.next(), self.next());
        NodeId::parse(&format!(
            "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
            a >> 32,
            (a >> 16) & 0xffff,
            a & 0xfff,
            (b >> 48) & 0xfff,
            b & 0xffff_ffff_ffff
        ))
        .unwrap()
    }
}

/// A queue's key: sender, receiver, document (as a string, for ordering).
type Lane = (usize, usize, String);

/// Per (sender, receiver, document) queues: in order per sender per
/// document, any order across documents and senders (yrs 0.27 does not
/// reliably integrate one sender's updates delivered out of order).
#[derive(Default)]
struct Network {
    queues: BTreeMap<Lane, VecDeque<(NodeId, Vec<u8>)>>,
}

impl Network {
    fn broadcast(&mut self, from: usize, replicas: usize, edit: &TreeEdit) {
        for to in (0..replicas).filter(|&r| r != from) {
            for (id, update) in &edit.touched {
                self.queues.entry((from, to, id.to_string())).or_default().push_back((*id, update.clone()));
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.queues.values().all(VecDeque::is_empty)
    }

    /// Deliver the next update of one random non-empty queue, then repair
    /// the receiver when the update touched its tree and broadcast that
    /// repair. Whether anything was delivered.
    fn deliver_one(&mut self, rng: &mut Rng, replicas: &mut [Tree], stats: &mut Stats) -> bool {
        let live: Vec<&Lane> = self.queues.iter().filter(|(_, q)| !q.is_empty()).map(|(k, _)| k).collect();
        let Some(&key) = live.get(rng.below(live.len().max(1))) else { return false };
        let key = key.clone();
        let (id, update) = self.queues.get_mut(&key).unwrap().pop_front().unwrap();
        let to = key.1;
        let effect = replicas[to].apply_update(id, &update).unwrap();
        stats.delivered += 1;
        if effect.structure {
            if let Some(repair) = checked_repair(&mut replicas[to], "repair", stats) {
                stats.repairs += 1;
                self.broadcast(to, replicas.len(), &repair);
            }
        }
        true
    }
}

/// A document's ancestors by stored `parent_id`s as the oracle reads
/// them (through `fields()`, not through the tree's own walk), and
/// whether the walk ended knowing everything above its start.
fn chain_of(tree: &Tree, start: NodeId) -> (Vec<NodeId>, bool) {
    let mut chain = Vec::new();
    let mut cur = start;
    loop {
        if chain.contains(&cur) {
            return (chain, true);
        }
        let Some(fields) = tree.doc(cur).and_then(|doc| doc.fields().ok()) else { return (chain, false) };
        chain.push(cur);
        match fields.parent_id.filter(|_| cur != tree.root()) {
            Some(parent) => cur = parent,
            None => return (chain, true),
        }
    }
}

fn carries_marker(tree: &Tree, id: NodeId) -> bool {
    tree.doc(id).and_then(|doc| doc.fields().ok()).is_some_and(|fields| fields.custom.contains_key(SHARE))
}

/// Per live node, the live nodes whose stored list names it.
fn naming_lists(tree: &Tree) -> HashMap<NodeId, Vec<NodeId>> {
    let mut naming: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for owner in sorted(tree.list_node_ids()) {
        for child in tree.doc(owner).unwrap().children() {
            if tree.has_node(child) && !naming.get(&child).is_some_and(|owners| owners.contains(&owner)) {
                naming.entry(child).or_default().push(owner);
            }
        }
    }
    naming
}

/// [`Tree::repair`] under the move contract's oracle: **a repair never
/// lists a node under a parent outside a share while a list in that share
/// names it.** Shares are judged as the contract says, on the documents
/// held when the repair runs. Two ways to break it, both checked:
/// the repair newly listed the node outside the share; or the node was in
/// two lists and the repair kept the outside one (allowed only when that
/// one is in a share the losing list is outside of: two shares cannot
/// both keep it). Breaking a cycle is its own rule and is left out.
fn checked_repair(tree: &mut Tree, now: &str, stats: &mut Stats) -> Option<TreeEdit> {
    let issues = tree.validate_tree();
    let in_a_cycle: HashSet<NodeId> = issues.iter().filter_map(|i| if let TreeIssue::Cycle { node_ids } = i { Some(node_ids.clone()) } else { None }).flatten().collect();
    stats.list_wins += issues.iter().filter(|i| matches!(i, TreeIssue::LeavesShare { .. })).count();
    let before = naming_lists(tree);
    // Where each listed node's `parent_id` pointed before the repair: a
    // list that agrees with it is honoured whatever other lists say (the
    // exception is judged only while the destination's list lacks the
    // node; see `analyze`).
    let stored_before: HashMap<NodeId, Option<NodeId>> =
        before.keys().map(|&n| (n, tree.doc(n).and_then(|d| d.fields().ok()).and_then(|f| f.parent_id))).collect();
    let placed_before: HashMap<NodeId, Option<NodeId>> =
        before.keys().map(|&n| (n, tree.doc(n).and_then(|d| d.fields().ok()).and_then(|f| f.placed_under))).collect();
    let chains: HashMap<NodeId, (Vec<NodeId>, bool)> = tree.list_node_ids().into_iter().map(|n| (n, chain_of(tree, n))).collect();
    let shares_of = |list: NodeId| -> Vec<NodeId> { chains[&list].0.iter().copied().filter(|&member| carries_marker(tree, member)).collect() };
    // (node, a list that names it, the shares that list is in)
    let mut facts: Vec<(NodeId, NodeId, Vec<NodeId>)> = Vec::new();
    for (&node, lists) in &before {
        for &list in lists {
            if !chains[&list].0.contains(&node) && !in_a_cycle.contains(&node) {
                facts.push((node, list, shares_of(list)));
            }
        }
    }
    let competing: HashMap<(NodeId, NodeId), bool> = facts
        .iter()
        .flat_map(|(node, list, _)| before[node].iter().map(move |other| (*list, *other)))
        .map(|(list, other)| ((list, other), shares_of(other).iter().any(|share| !chains[&list].0.contains(share))))
        .collect();

    let repair = tree.repair(now).unwrap();
    if let Some(edit) = &repair {
        stats.trail.push_back((issues.clone(), edit.touched.iter().map(|(id, _)| *id).collect()));
        if stats.trail.len() > 6 {
            stats.trail.pop_front();
        }
    }
    if repair.is_some() {
        let after = naming_lists(tree);
        for (node, list, shares) in &facts {
            let Some(now_in) = after.get(node) else { continue };
            for holder in now_in.iter().filter(|holder| *holder != list) {
                let (chain, complete) = &chains[holder];
                let Some(share) = shares.iter().find(|share| !chain.contains(share)) else { continue };
                if !complete {
                    continue;
                }
                let newly = !before[node].contains(holder);
                // Honoured: the `parent_id` named the holder and was
                // placed there, or the holder's list already named it.
                if stored_before[node] == Some(*holder) && (placed_before[node] == Some(*holder) || !newly) {
                    continue;
                }
                let kept_instead = !newly && !now_in.contains(list) && !competing[&(*list, *holder)];
                let short = |id: &NodeId| id.to_string()[..8].to_string();
                let describe = |id: &NodeId| {
                    let (chain, complete) = chain_of(tree, *id);
                    format!(
                        "{}{} chain [{}] complete {complete}",
                        short(id),
                        if carries_marker(tree, *id) { "*" } else { "" },
                        chain.iter().map(|n| format!("{}{}", short(n), if carries_marker(tree, *n) { "*" } else { "" })).collect::<Vec<_>>().join(" > ")
                    )
                };
                assert!(
                    !newly && !kept_instead,
                    "repair listed {} under {}, outside share {}, while {} named it (newly: {newly}, kept_instead: {kept_instead});\n stored {:?} placed {:?};\n lists before {:?} after {:?};\n holder: {};\n list: {};\n stored parent: {}",
                    short(node),
                    short(holder),
                    short(share),
                    short(list),
                    stored_before[node].as_ref().map(short),
                    placed_before[node].as_ref().map(short),
                    before[node].iter().map(short).collect::<Vec<_>>(),
                    now_in.iter().map(short).collect::<Vec<_>>(),
                    describe(holder),
                    describe(list),
                    stored_before[node].as_ref().map(describe).unwrap_or_else(|| "none".into())
                );
            }
        }
    }
    repair
}

#[derive(Default, Debug)]
struct Stats {
    /// The last repairs: the issues found and the documents touched, for
    /// reading a ping-pong when the drain cap fires.
    trail: std::collections::VecDeque<(Vec<TreeIssue>, Vec<NodeId>)>,
    delivered: usize,
    repairs: usize,
    adds: usize,
    moves: usize,
    removes: usize,
    undeletes: usize,
    renames: usize,
    content_edits: usize,
    refused: usize,
    shares: usize,
    unshares: usize,
    transplants: usize,
    old_client_moves: usize,
    tampers: usize,
    list_wins: usize,
}

fn sorted(mut ids: Vec<NodeId>) -> Vec<NodeId> {
    ids.sort_by_key(|id| id.to_string());
    ids
}

fn run_convergence(seed: u64, steps: usize) -> Stats {
    // Every document's client id follows the seed too, so a seed names
    // one trajectory: which concurrent inserts win is decided by client
    // id inside yrs.
    crate::node_doc::seed_client_ids(seed * 1_000_000 + 1);
    const REPLICAS: usize = 3;
    let mut rng = Rng::new(seed);
    let root = rng.node_id();
    let first = Tree::new(root, "Fuzz", "t0").unwrap();
    let mut replicas: Vec<Tree> = (0..REPLICAS).map(|_| replica_of(&first)).collect();
    let mut net = Network::default();
    let mut stats = Stats::default();
    let mut created: Vec<NodeId> = Vec::new();

    for step in 0..steps {
        let now = format!("2026-09-18T00:00:00.{step:06}Z");
        let r = rng.below(REPLICAS);
        // A third of the steps deliver something in flight; when nothing
        // is, the step makes an edit instead.
        if rng.below(100) < 35 && net.deliver_one(&mut rng, &mut replicas, &mut stats) {
            continue;
        }
        let tree = &mut replicas[r];
        let nodes = sorted(tree.list_node_ids());
        let non_root: Vec<NodeId> = nodes.iter().copied().filter(|&n| n != root).collect();
        let dead = |tree: &Tree| -> Vec<NodeId> {
            sorted(tree.ids()).into_iter().filter(|&id| tree.doc(id).unwrap().fields().ok().is_some_and(|f| f.deleted_at.is_some())).collect()
        };
        let roll = rng.below(100);
        let result = if roll < 24 {
            let id = rng.node_id();
            let parent = rng.pick(&nodes).unwrap();
            let place = match rng.below(3) {
                0 => Place::Last,
                1 => Place::First,
                _ => tree.get_children(parent).ok().and_then(|kids| rng.pick(&kids)).map_or(Place::Last, Place::After),
            };
            let edit = tree.add_node(id, Some(parent), place, "document", &format!("n{step}"), &now);
            if edit.is_ok() {
                created.push(id);
                stats.adds += 1;
            }
            edit
        } else if roll < 46 {
            // Every move an honest client makes: a plain move, or a
            // transplant when it would leave a share.
            match (rng.pick(&non_root), rng.pick(&nodes)) {
                (Some(id), Some(parent)) => {
                    // Every kind of place, `After` a child of the
                    // destination or a node anywhere (an anchor the
                    // list does not name means the end).
                    let place = match rng.below(4) {
                        0 => Place::Last,
                        1 => Place::First,
                        2 => tree.get_children(parent).ok().and_then(|kids| rng.pick(&kids)).map_or(Place::Last, Place::After),
                        _ => rng.pick(&nodes).map_or(Place::Last, Place::After),
                    };
                    let mut made = Vec::new();
                    let moved = tree.move_or_transplant(HERE, id, parent, place, &now, &mut || {
                        made.push(rng.node_id());
                        *made.last().unwrap()
                    });
                    match moved {
                        Ok((now_id, edit)) => {
                            if now_id == id {
                                stats.moves += 1;
                            } else {
                                stats.transplants += 1;
                            }
                            created.extend(made);
                            Ok(edit)
                        }
                        Err(e) => Err(e),
                    }
                }
                _ => continue,
            }
        } else if roll < 54 {
            let Some(id) = rng.pick(&non_root) else { continue };
            stats.removes += 1;
            tree.remove_node(id, &now)
        } else if roll < 60 {
            let Some(id) = rng.pick(&dead(tree)) else { continue };
            stats.undeletes += 1;
            tree.undelete_node(id, &now)
        } else if roll < 66 {
            let Some(id) = rng.pick(&nodes) else { continue };
            stats.renames += 1;
            tree.set_title(id, &format!("t{step}"), &now)
        } else if roll < 74 {
            // A folder is shared, or a share is stopped.
            let Some(id) = rng.pick(&non_root) else { continue };
            if carries_marker(tree, id) {
                stats.unshares += 1;
                tree.remove_custom(id, SHARE, &now)
            } else if nodes.iter().filter(|&&n| carries_marker(tree, n)).count() < 4 {
                stats.shares += 1;
                tree.set_custom(id, SHARE, &marker(&format!("s{step}")), &now)
            } else {
                continue;
            }
        } else if roll < 78 {
            // A client from before the contract: a plain move, wherever
            // to, and no `placed_under` written (it did not know the key).
            match (rng.pick(&non_root), rng.pick(&nodes)) {
                (Some(id), Some(parent)) => match tree.move_node(id, parent, Place::Last, &now) {
                    Ok(mut edit) => {
                        stats.old_client_moves += 1;
                        let (_, update) = tree.docs[&id]
                            .edit_node(|d, txn| {
                                d.clear_placement(txn);
                                Ok(())
                            })
                            .unwrap();
                        edit.push(id, update);
                        Ok(edit)
                    }
                    Err(e) => Err(e),
                },
                _ => continue,
            }
        } else if roll < 90 {
            // A client that does not keep the rule: a `parent_id` written
            // on its own, pointing at any node (out of the node's share,
            // more often than not), a tombstone, the node itself, nothing.
            let Some(id) = rng.pick(&non_root) else { continue };
            let target = match rng.below(10) {
                0 => Some(id),
                1 => None,
                2 | 3 => rng.pick(&dead(tree)).or(Some(root)),
                _ => rng.pick(&nodes),
            };
            stats.tampers += 1;
            Ok(tamper_parent(tree, id, target))
        } else {
            let Some(id) = rng.pick(&nodes) else { continue };
            match tree.doc_mut(id).unwrap().replace_plain_text(&format!("text {step}\nline")) {
                Ok(update) => {
                    stats.content_edits += 1;
                    Ok(TreeEdit { touched: vec![(id, update)] })
                }
                Err(e) => Err(e),
            }
        };
        match result {
            Ok(edit) => net.broadcast(r, REPLICAS, &edit),
            Err(_) => stats.refused += 1,
        }
    }

    // Everything delivered, repairs included, then rounds of repair and
    // exchange until nothing moves.
    for round in 0..10 {
        // A drain that never ends is repairs answering each other's
        // repairs; a whole run delivers some thousands of updates.
        let mut drained = 0usize;
        while net.deliver_one(&mut rng, &mut replicas, &mut stats) {
            drained += 1;
            if drained >= 50_000 {
                // The documents the last repairs touched or judged, on every replica.
                let mut involved: Vec<NodeId> = Vec::new();
                for (issues, touched) in &stats.trail {
                    for issue in issues {
                        match issue {
                            TreeIssue::LeavesShare { parent_id, child_id, stored_parent } => {
                                involved.extend([*parent_id, *child_id]);
                                involved.extend(*stored_parent);
                            }
                            TreeIssue::MissingFromList { parent_id, child_id } | TreeIssue::WrongList { parent_id, child_id, .. } => {
                                involved.extend([*parent_id, *child_id]);
                            }
                            _ => {}
                        }
                    }
                    involved.extend(touched.iter().copied());
                }
                involved = sorted(involved);
                involved.dedup();
                let mut dump = String::new();
                for (r, tree) in replicas.iter().enumerate() {
                    dump.push_str(&format!("\nreplica {r}:"));
                    for &id in &involved {
                        let Some(doc) = tree.doc(id) else { dump.push_str(&format!("\n  {id}: not held")); continue };
                        let fields = doc.fields().unwrap();
                        let chain = chain_of(tree, id).0.iter().map(|n| n.to_string()[..8].to_string()).collect::<Vec<_>>().join(" > ");
                        dump.push_str(&format!(
                            "\n  {}: parent {:?} deleted {} marker {} listed_by {:?} children {:?} chain {chain}",
                            &id.to_string()[..8],
                            fields.parent_id.map(|p| p.to_string()[..8].to_string()),
                            fields.deleted_at.is_some(),
                            fields.custom.contains_key(SHARE),
                            naming_lists(tree).get(&id).map(|l| l.iter().map(|n| n.to_string()[..8].to_string()).collect::<Vec<_>>()),
                            doc.children().iter().map(|n| n.to_string()[..8].to_string()).collect::<Vec<_>>()
                        ));
                    }
                }
                panic!("seed {seed}: the network never drained in round {round}: {stats:?}; issues per replica: {:?}{dump}", replicas.iter().map(Tree::validate_tree).collect::<Vec<_>>());
            }
        }
        let mut quiet = true;
        for (r, tree) in replicas.iter_mut().enumerate() {
            if let Some(repair) = checked_repair(tree, "final", &mut stats) {
                quiet = false;
                stats.repairs += 1;
                net.broadcast(r, REPLICAS, &repair);
            }
        }
        if quiet && net.is_empty() {
            break;
        }
        assert!(round < 9, "seed {seed}: repairs did not settle");
    }

    for (r, tree) in replicas.iter_mut().enumerate() {
        let issues = tree.validate_tree();
        assert!(issues.is_empty(), "seed {seed} replica {r}: {issues:?}");
        assert!(tree.repair("check").unwrap().is_none(), "seed {seed} replica {r}: repair not settled");
        for &id in &created {
            let fields = tree.doc(id).unwrap_or_else(|| panic!("seed {seed} replica {r}: lost {id}")).fields().unwrap();
            assert!(tree.has_node(id) || fields.deleted_at.is_some(), "seed {seed} replica {r}: {id} neither present nor tombstoned");
        }
    }
    for r in 1..REPLICAS {
        let (first, rest) = replicas.split_at_mut(r);
        assert_same(&first[0], &rest[0]);
    }
    stats
}

/// Eight seeds in a release build (five seconds); one in a debug build,
/// where the oracle in `checked_repair` (chains and naming lists
/// recomputed at every one of some thousand repairs per seed) takes
/// minutes per seed. `cargo test --release -p pimble-crdt` runs the lot.
#[test]
fn three_replicas_converge_under_random_edits_delivered_in_random_order() {
    let started = std::time::Instant::now();
    // `PIMBLE_CONVERGENCE_SEEDS=1,5` runs exactly those seeds, in either build.
    let chosen: Vec<u64> = std::env::var("PIMBLE_CONVERGENCE_SEEDS")
        .ok()
        .map(|list| list.split(',').filter_map(|seed| seed.trim().parse().ok()).collect())
        .unwrap_or_default();
    let seeds: &[u64] = if !chosen.is_empty() {
        &chosen
    } else if cfg!(debug_assertions) {
        &[2]
    } else {
        &[1, 2, 3, 4, 5, 6, 7, 8]
    };
    let (mut transplants, mut list_wins) = (0, 0);
    for &seed in seeds {
        let mut stats = run_convergence(seed, 400);
        // The trail is for reading a failure; a passing seed prints its counts.
        stats.trail.clear();
        assert!(stats.adds > 20 && stats.moves > 10 && stats.removes > 5 && stats.tampers > 10 && stats.shares > 3, "seed {seed} did too little: {stats:?}");
        transplants += stats.transplants;
        list_wins += stats.list_wins;
        eprintln!("seed {seed}: {stats:?}");
    }
    // Floors per seed, set from what eight seeds do: a transplant is a
    // move that happens to leave a share, and the exception fires only
    // for a `parent_id` written on its own whose parent does not list
    // the node, which `placed_under` made rare (ten per seed was too
    // many for CI's eight, 70 in all, on 2026-09-22).
    let (least_transplants, least_wins) = (2 * seeds.len(), 3 * seeds.len());
    assert!(
        transplants >= least_transplants && list_wins >= least_wins,
        "the move contract was hardly exercised: {transplants} transplants, {list_wins} lists that won"
    );
    eprintln!("randomized convergence over {} seed(s): {:?}", seeds.len(), started.elapsed());
}
