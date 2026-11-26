use std::collections::{BTreeMap, HashMap, HashSet};

use pimble_core::NodeId;

use crate::error::Result;

use super::{Tree, TreeEdit, ShareClaim, DocRepair, TreeAnalysis, TreeIssue};

impl Tree {
    // ── Shape ────────────────────────────────────────────────────────────

    /// Exactly the conditions [`Tree::repair`] fixes; empty after a repair.
    /// `OrphanNode` and `MissingChild` name a document this device KNOWS is
    /// deleted (a tombstone it holds). One it does not hold, or holds without
    /// its `node` root, is unknown here and is never reported or "fixed":
    /// see `analyze`.
    pub fn validate_tree(&self) -> Vec<TreeIssue> {
        self.analyze().issues
    }

    /// Decision 9 over documents: effective parents (a node's `parent_id`,
    /// except that a list which still names it wins over a `parent_id` that
    /// would take it out of a share: docs/MOVE_CONTRACT.md "Repair"), cycles
    /// broken at the smallest id, every list made to hold the undeleted nodes
    /// whose effective parent is its owner (first occurrence kept, missing
    /// ones appended in id order), tombstones and duplicates taken out, and
    /// every entry naming a document not held here left exactly where it is.
    /// Deterministic in the held state, and acting on knowledge only, so two
    /// devices that hold different subsets never undo each other. `None`
    /// when nothing needed fixing.
    ///
    /// Touches only `parent_id` and children lists, never a timestamp: the
    /// clock rule of the share mirror served a causal guard that no longer
    /// exists (docs/NODE_DOCUMENT_CONTRACT.md section 2), so `now` is unused.
    pub fn repair(&mut self, _now: &str) -> Result<Option<TreeEdit>> {
        let analysis = self.analyze();
        if analysis.parent_rewrites.is_empty() && analysis.list_rewrites.is_empty() {
            return Ok(None);
        }
        // One transaction per document, whatever it needs: its `parent_id`,
        // its list, or both. Ordered by id so two replicas produce the same
        // edit in the same order.
        let mut per_doc: BTreeMap<String, DocRepair> = BTreeMap::new();
        for (id, parent, placed) in analysis.parent_rewrites {
            per_doc.entry(id.to_string()).or_insert((id, None, Vec::new(), Vec::new())).1 = Some((parent, placed));
        }
        for (owner, removals, appends) in analysis.list_rewrites {
            let entry = per_doc.entry(owner.to_string()).or_insert((owner, None, Vec::new(), Vec::new()));
            entry.2 = removals;
            entry.3 = appends;
        }
        let mut edit = TreeEdit::default();
        for (id, parent, removals, appends) in per_doc.into_values() {
            let (_, update) = self.docs[&id].edit(|d, txn| {
                if let Some((parent, placed)) = parent {
                    if placed {
                        d.write_parent(txn, Some(parent));
                    } else {
                        d.write_parent_only(txn, Some(parent));
                    }
                }
                for index in removals {
                    d.list_remove_at(txn, index)?;
                }
                for child in appends {
                    d.list_insert(txn, None, child);
                }
                Ok(())
            })?;
            edit.push(id, update);
        }
        Ok((!edit.is_empty()).then_some(edit))
    }

    /// Read-only pass computing effective parents (after cycle resolution),
    /// every `TreeIssue` they and the children lists reveal, and exactly the
    /// writes `repair` would make. A tree whose root is not a node yet (a
    /// fresh replica before its first reconcile) analyzes as empty.
    pub(super) fn analyze(&self) -> TreeAnalysis {
        let mut analysis = TreeAnalysis { issues: Vec::new(), parent_rewrites: Vec::new(), list_rewrites: Vec::new() };
        let root = self.root;
        if !self.has_node(root) {
            return analysis;
        }

        // The nodes with their stored parents, in id order so every list
        // below is deterministic.
        let mut nodes: Vec<(NodeId, Option<NodeId>)> = self
            .docs
            .iter()
            // Attachments are in no list and have no parent: never placed.
            .filter(|(_, doc)| doc.attached_to().is_none())
            .filter_map(|(&id, doc)| doc.placement().map(|parent| (id, parent)))
            .collect();
        nodes.sort_by_key(|(id, _)| id.to_string());
        let node_ids: Vec<NodeId> = nodes.iter().map(|(id, _)| *id).collect();
        let node_set: HashSet<NodeId> = node_ids.iter().copied().collect();
        // What this device KNOWS is deleted. Everything else that is not a
        // node (a document not held, or held and not initialised) is unknown
        // here, which is not the same as missing: node documents travel one
        // by one, a list can name a child before the child's document is
        // here, and a device may hold a document it has no key for yet (a
        // member's new document on an owner's page, until the share's key or
        // the store key's wrap reaches it). Repair acts on knowledge only. A
        // device that "fixed" what it merely lacks would be undone by every
        // device that holds it, for ever, and would move or unlist other
        // people's documents meanwhile (found 2026-09-21: an owner's page
        // unlisting a member's new note six times a second).
        let gone: HashSet<NodeId> = self.docs.iter().filter(|(_, doc)| doc.is_tombstone()).map(|(&id, _)| id).collect();

        // Every node's list as stored, read once, and from them the nodes
        // whose list names each id (in id order, each once): what step 1
        // needs to know before it believes a `parent_id`.
        let lists: HashMap<NodeId, Vec<Option<NodeId>>> = node_ids.iter().map(|&owner| (owner, self.docs[&owner].raw_children())).collect();
        let mut listed_by: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for &owner in &node_ids {
            for &child in lists[&owner].iter().flatten() {
                let owners = listed_by.entry(child).or_default();
                if owners.last() != Some(&owner) {
                    owners.push(owner);
                }
            }
        }

        // What each list's own document says left it (see `LEFT_PREFIX`).
        let left: HashMap<NodeId, HashSet<NodeId>> = node_ids.iter().map(|&owner| (owner, self.docs[&owner].left_children())).filter(|(_, l)| !l.is_empty()).collect();

        // Step 1: effective parent for every node but the root.
        let mut raw_parent: HashMap<NodeId, Option<NodeId>> = HashMap::new();
        let mut effective_parent: HashMap<NodeId, NodeId> = HashMap::new();
        for &(id, parent) in &nodes {
            if id == root {
                continue;
            }
            raw_parent.insert(id, parent);
            // The parent it names says it left, and its list does not name it:
            // this copy of the node is older than that parent's (the move or
            // the deletion that took it out has not reached this device).
            // Where it is now is not known here, so nothing is decided about
            // it: no listing, no rewrite, no other list's entry judged by it
            // (2026-11-19: a member's device lacking a note's tombstone put
            // it back in the share's list after every repair the owner's
            // made, thousands of times).
            if let Some(p) = parent {
                let names_it = lists.get(&p).is_some_and(|l| l.contains(&Some(id)));
                if !names_it && left.get(&p).is_some_and(|l| l.contains(&id)) {
                    continue;
                }
            }
            // Where its `parent_id` puts the node, with the issue that says
            // so when that is not simply "under the node it names".
            let (destination, issue) = match parent {
                None => (root, Some(TreeIssue::DetachedNode { node_id: id })),
                Some(p) if node_set.contains(&p) && p != id => (p, None),
                // Its own parent, or under a document known to be deleted.
                Some(p) if p == id || gone.contains(&p) => (root, Some(TreeIssue::OrphanNode { node_id: id, missing_parent: p })),
                // Under a document this device does not hold: where it
                // belongs is not known here, so it is left exactly where it
                // says it is (no effective parent, no rewrite, no listing).
                Some(_) => continue,
            };
            // The one exception to "`parent_id` is the truth of where a node
            // is" (docs/MOVE_CONTRACT.md "Repair"): a list that still names
            // a node wins over a `parent_id` that would take it out of a
            // share. No operation makes such a move (one that leaves a share
            // is a transplant: the node stays and is tombstoned), so a
            // `parent_id` that says so was written by a client that does not
            // keep the rule, and completing its move would put a member's
            // node in the owner's private tree, out of every member's reach.
            // Every device that holds the list, the node, the marker and
            // what is above the destination decides the same, from the same
            // documents; one that does not hold all of the last decides
            // nothing (below), which is the 2026-09-21 rule again.
            //
            // A `parent_id` is honoured, whatever other lists say, when it was
            // placed (`placed_under` agrees with it: the write came from an
            // operation that also edited the lists) or when the destination's
            // own list already names the node. Only a `parent_id` written on
            // its own, whose destination does not list the node, is judged;
            // and a judgement writes a placement, so every device honours it
            // afterwards and nothing rewrites it again. That bound is what
            // keeps repair convergent. Without it (found 2026-09-22 by the
            // randomized test, seeds 76 and 1), two devices that had nested
            // two shared folders into each other by concurrent moves each sat
            // in a consistent tree of its own and rewrote the other's rewrite
            // for ever: a device judged by the ancestry of the destination
            // and by its lists, both of which the other device's repair was
            // changing at the same time. A client that writes `parent_id`
            // and `placed_under` together, leaving the old list naming the
            // node, gets its move completed instead; the node stays in the
            // share's scope (a scope set only grows), the share's members see
            // it in "Recently Deleted..." as a node no list of theirs names,
            // and any editor puts it back: the residual the contract accepts
            // of a client that unlists the node.
            let lists_naming = listed_by.get(&id).map(Vec::as_slice).unwrap_or_default();
            // Both kinds of evidence speak only for the parent the node
            // names. A stored parent that is gone or missing sends the node
            // to the root, which no operation placed it under, and a list of
            // the root's that happens to name it (another device's adoption)
            // is no evidence either: that is judged by the lists like any
            // other `parent_id` written on its own.
            let honoured = parent == Some(destination)
                && (self.docs[&id].placed_under() == parent || lists_naming.contains(&destination));
            let claim = if honoured { ShareClaim::None } else { self.share_claim(id, destination, lists_naming) };
            let e = match claim {
                ShareClaim::Wins(list) => {
                    analysis.issues.push(TreeIssue::LeavesShare { parent_id: list, child_id: id, stored_parent: parent });
                    list
                }
                // Whether the destination is outside the share is not known
                // here: some document above it is not held. Believing the
                // `parent_id` would unlist the node from the share, which
                // no later repair puts right; so nothing is decided until
                // the documents are here, as for a parent that is not held.
                ShareClaim::Unknown => continue,
                ShareClaim::None => {
                    analysis.issues.extend(issue);
                    destination
                }
            };
            effective_parent.insert(id, e);
        }

        // Step 2: break every cycle in the effective-parent graph by
        // redirecting the smallest-id member (string order) of each to the root.
        let bound = node_ids.len();
        let mut visited: HashSet<NodeId> = HashSet::new();
        let mut cycle_broken: HashSet<NodeId> = HashSet::new();
        for &start in &node_ids {
            if start == root || visited.contains(&start) {
                continue;
            }
            let mut cur = start;
            let mut reaches_root = false;
            for _ in 0..=bound {
                if cur == root {
                    reaches_root = true;
                    break;
                }
                // A chain that ends at a node whose parent is not held ends:
                // it is no cycle.
                match effective_parent.get(&cur) {
                    Some(&next) => cur = next,
                    None => {
                        reaches_root = true;
                        break;
                    }
                }
            }
            if reaches_root {
                continue;
            }
            // `start`'s chain loops without reaching the root. Walk it again,
            // recording the path, to find exactly the cycle's members (a walk
            // from a node outside the cycle first crosses a tail that needs
            // no change of its own once the cycle it feeds is broken).
            let mut path = Vec::new();
            let mut index_of: HashMap<NodeId, usize> = HashMap::new();
            let mut cur = start;
            loop {
                if let Some(&i) = index_of.get(&cur) {
                    let members: Vec<NodeId> = path[i..].to_vec();
                    let smallest = members
                        .iter()
                        .min_by_key(|id| id.to_string())
                        .copied()
                        .expect("a cycle has at least one member");
                    effective_parent.insert(smallest, root);
                    cycle_broken.insert(smallest);
                    analysis.issues.push(TreeIssue::Cycle { node_ids: members });
                    break;
                }
                index_of.insert(cur, path.len());
                path.push(cur);
                visited.insert(cur);
                // Every member of a chain that did not end has one (the walk
                // above would have ended at the first that does not).
                let Some(&next) = effective_parent.get(&cur) else { break };
                cur = next;
            }
        }

        // Step 3: `parent_id` rewrites wherever the stored value differs from
        // the (possibly cycle-corrected) effective parent.
        for &id in &node_ids {
            if id == root {
                continue;
            }
            let Some(&e) = effective_parent.get(&id) else { continue };
            if raw_parent[&id] != Some(e) {
                analysis.parent_rewrites.push((id, e, !cycle_broken.contains(&id)));
            }
        }

        // Steps 4-5: surgery on every node's list. Only nodes' lists: a
        // tombstoned document keeps its list for an undelete, and a
        // content-only document has nothing to keep in order.
        let mut remove_indices: HashMap<NodeId, Vec<usize>> = HashMap::new();
        let mut placed: HashSet<NodeId> = HashSet::new();
        for &owner in &node_ids {
            let mut seen: HashSet<NodeId> = HashSet::new();
            for (index, &entry) in lists[&owner].iter().enumerate() {
                let Some(child) = entry else {
                    remove_indices.entry(owner).or_default().push(index);
                    continue;
                };
                if child == root {
                    analysis.issues.push(TreeIssue::RootInList { parent_id: owner });
                    remove_indices.entry(owner).or_default().push(index);
                    continue;
                }
                if gone.contains(&child) {
                    analysis.issues.push(TreeIssue::MissingChild { parent_id: owner, child_id: child });
                    remove_indices.entry(owner).or_default().push(index);
                    continue;
                }
                if !seen.insert(child) {
                    analysis.issues.push(TreeIssue::DuplicateInList { parent_id: owner, child_id: child });
                    remove_indices.entry(owner).or_default().push(index);
                    continue;
                }
                // Not held here, or a node whose own parent is not held:
                // unknown, so the entry stays where it is.
                let Some(&e) = effective_parent.get(&child) else { continue };
                if e != owner {
                    analysis.issues.push(TreeIssue::WrongList { parent_id: owner, child_id: child, effective_parent: e });
                    remove_indices.entry(owner).or_default().push(index);
                    continue;
                }
                placed.insert(child);
            }
        }

        let mut appends: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for &child in node_ids.iter().filter(|&&id| id != root && !placed.contains(&id)) {
            let Some(&owner) = effective_parent.get(&child) else { continue };
            analysis.issues.push(TreeIssue::MissingFromList { parent_id: owner, child_id: child });
            appends.entry(owner).or_default().push(child);
        }

        for &owner in &node_ids {
            let mut removals = remove_indices.remove(&owner).unwrap_or_default();
            let owner_appends = appends.remove(&owner).unwrap_or_default();
            if removals.is_empty() && owner_appends.is_empty() {
                continue;
            }
            removals.sort_unstable_by(|a, b| b.cmp(a));
            analysis.list_rewrites.push((owner, removals, owner_appends));
        }

        analysis
    }
}
