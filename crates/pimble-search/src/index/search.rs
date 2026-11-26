//! Queries: keyword (BM25 over title, text and headings), semantic (nearest
//! chunks) and the reciprocal-rank-fusion blend of the two.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use pimble_core::NodeId;
use rhypedb_engine::object::Object;
use rhypedb_engine::vectorizer::SimilarHit;

use super::errors::{map_engine_err, map_fulltext_err};
use super::hits::{chunk_hit, keyword_hit, Exact};
use super::prefix::with_trailing_prefix;
use super::SearchIndex;
use crate::error::{Result, SearchError};
use crate::query::{SearchHit, SearchQuery};

/// Cosine-distance ceiling for a semantic hit to count as a real match at
/// all, rather than "closest of a bad lot." Measured against
/// `all-MiniLM-L6-v2`'s embeddings (the only model this crate's schema
/// names): topically related passages land around 0.3-0.5 cosine distance,
/// unrelated ones around 0.7 and up, with a fuzzy band in between. 0.65 sits
/// in that band, erring toward still admitting a loosely-related hit rather
/// than a strict "must be closely related" cutoff. Without this, a query
/// with no real semantic match in the index still gets `k` hits — whichever
/// chunks happened to be least-far, however unrelated — because ANN search
/// always returns its `k` best candidates, never "none."
const SEMANTIC_MAX_DISTANCE: f32 = 0.65;

/// How much a title match counts against a match in the text.
const TITLE_WEIGHT: f32 = 2.0;
/// How much a heading match counts against a match in the text (the same
/// words are in the text too, so this is on top of it).
const HEADING_WEIGHT: f32 = 1.5;

impl SearchIndex {
    /// Run `q` and return up to `q.limit` hits: keyword-only when
    /// `q.semantic` is false or this index has no vectorizer, otherwise a
    /// reciprocal-rank-fusion (k = 60) blend of keyword and semantic results.
    ///
    /// Degrades to keyword-only (rather than failing the query) if the
    /// embedding model isn't available right now — still downloading, in
    /// load-retry backoff, or never warmed up — logging a warning the first
    /// time that happens for this index, not on every subsequent query.
    pub fn search(&self, q: &SearchQuery) -> Result<Vec<SearchHit>> {
        let keyword_hits = self.keyword_search(&q.text, q.limit.max(1))?;
        if !q.semantic || self.vectorizer.is_none() {
            let mut hits = keyword_hits;
            hits.truncate(q.limit);
            return Ok(hits);
        }
        let semantic_hits = match self.semantic_search(&q.text, (q.limit * 3).max(1)) {
            Ok(hits) => hits,
            Err(SearchError::ModelUnavailable(msg)) => {
                if !self.model_unavailable_warned.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        "Semantic search unavailable ({msg}); degrading hybrid queries to \
                         keyword-only until the embedding model loads"
                    );
                }
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        Ok(hybrid_merge(keyword_hits, semantic_hits, q.limit))
    }

    /// Run the same full-text query over `title`, `text` and `headings`.
    fn fulltext_all(&self, query: &str, k: usize) -> Result<[Vec<rhypedb_engine::fulltext::FulltextHit>; 3]> {
        let field = |name: &str| -> Result<Vec<rhypedb_engine::fulltext::FulltextHit>> {
            Ok(self.db.fulltext_search("Node", name, query, k, None, None).map_err(map_fulltext_err)?.hits)
        };
        Ok([field("title")?, field("text")?, field("headings")?])
    }

    fn keyword_search(&self, query_text: &str, limit: usize) -> Result<Vec<SearchHit>> {
        if query_text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let k = (limit * 4).max(20);

        // Search-as-you-type: the last word the user is still typing is a prefix
        // term. If the engine refuses the expansion (too short after analysis, or
        // more than its cap of distinct terms), fall back to the literal query.
        let typed = with_trailing_prefix(query_text);
        let [title_hits, text_hits, heading_hits] = match self.fulltext_all(&typed, k) {
            Ok(hits) => hits,
            Err(_) if typed != query_text => self.fulltext_all(query_text, k)?,
            Err(e) => return Err(e),
        };

        let mut combined: HashMap<u64, f32> = HashMap::new();
        for h in &title_hits {
            *combined.entry(h.object_id).or_insert(0.0) += h.score * TITLE_WEIGHT;
        }
        for h in &text_hits {
            *combined.entry(h.object_id).or_insert(0.0) += h.score;
        }
        for h in &heading_hits {
            *combined.entry(h.object_id).or_insert(0.0) += h.score * HEADING_WEIGHT;
        }

        let mut ranked: Vec<(u64, f32)> = combined.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        // Every candidate is judged for an exact title or heading before the
        // cut, so one that BM25 put low still comes up.
        ranked.truncate(k);

        let ids: Vec<u64> = ranked.iter().map(|(id, _)| *id).collect();
        let objects_by_id: HashMap<u64, Object> = self
            .db
            .get_many("Node", &ids)
            .map_err(map_engine_err)?
            .into_iter()
            .map(|o| (o.id, o))
            .collect();

        let wanted = pimble_crdt::markdown::loose(query_text);
        let mut hits: Vec<(Exact, SearchHit)> = ranked
            .into_iter()
            .filter_map(|(id, score)| {
                let obj = objects_by_id.get(&id)?;
                keyword_hit(obj, score, query_text, &wanted)
            })
            .collect();
        // A note titled exactly the query first, then one with a heading that
        // is exactly it (ignoring case and punctuation), then the rest; each
        // group in score order (the sort is stable).
        hits.sort_by_key(|(exact, _)| std::cmp::Reverse(*exact));
        hits.truncate(limit);
        Ok(hits.into_iter().map(|(_, hit)| hit).collect())
    }

    fn semantic_search(&self, query_text: &str, k: usize) -> Result<Vec<SearchHit>> {
        let Some(vectorizer) = &self.vectorizer else {
            return Ok(Vec::new());
        };
        if query_text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let ef = k.max(64);
        // `exact_rescore: false` — the cheap ANN (TurboQuant) distance
        // estimate, not the full-precision-over-the-LSM rescore. Returns
        // `EngineError::ModelUnavailable` (mapped to
        // `SearchError::ModelUnavailable` below) rather than blocking if the
        // model hasn't loaded yet; `search`'s caller degrades to
        // keyword-only on that specific error.
        // The query is embedded once for every store's index (`crate::embed`).
        let query_vec = crate::embed::embed_query(query_text)?;
        let candidates = vectorizer
            .search_vector("Chunk", "embedding", &query_vec, k, ef, false, None)
            .map_err(map_engine_err)?;
        // Drop anything too far to count as a real match before grouping by
        // node, so a query with no real semantic match in the index yields
        // few or no semantic hits instead of `k` random ones (ANN search
        // always returns its `k` best candidates, never "none" — this is
        // what turns "best of a bad lot" into "none of these are good
        // enough"). See SEMANTIC_MAX_DISTANCE's doc comment.
        let candidates = drop_hits_beyond_max_distance(candidates);
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let chunk_ids: Vec<u64> = candidates.iter().map(|hit| hit.object_id).collect();
        let chunk_by_id: HashMap<u64, Object> = self
            .db
            .get_many("Chunk", &chunk_ids)
            .map_err(map_engine_err)?
            .into_iter()
            .map(|o| (o.id, o))
            .collect();

        // `candidates` already comes back in rank order — `rerank_score`
        // descending where the (disabled-by-default, see `SearchIndex::open`)
        // cross-encoder ran, else `distance` ascending; see `SimilarHit`'s
        // doc comment in rhypedb-engine. So keeping the FIRST chunk seen per
        // node, in this order, is that node's best chunk by the engine's own
        // ranking — no re-sort belongs here. Re-sorting this list by
        // `distance` (as this function used to) is exactly the bug fixed by
        // rhypedb #18's `SimilarHit`: doing that against a list the
        // cross-encoder had already ranked by an unrelated, higher-is-better
        // relevance score put the least-relevant chunk first.
        let mut seen_nodes: HashSet<u64> = HashSet::new();
        let mut best_per_node: Vec<(u64, f32, u64)> = Vec::new(); // (node_id, distance, chunk_id)
        for hit in &candidates {
            let node_id = self
                .db
                .get_links("Chunk", hit.object_id, "node")
                .map_err(map_engine_err)?
                .into_iter()
                .next()
                .map(|(id, _)| id);
            let Some(node_id) = node_id else { continue };
            if seen_nodes.insert(node_id) {
                best_per_node.push((node_id, hit.distance, hit.object_id));
            }
        }

        let node_ids: Vec<u64> = best_per_node.iter().map(|(node_id, ..)| *node_id).collect();
        let node_by_id: HashMap<u64, Object> = self
            .db
            .get_many("Node", &node_ids)
            .map_err(map_engine_err)?
            .into_iter()
            .map(|o| (o.id, o))
            .collect();

        Ok(best_per_node
            .into_iter()
            .filter_map(|(node_id, distance, chunk_id)| {
                let node_obj = node_by_id.get(&node_id)?;
                let chunk_obj = chunk_by_id.get(&chunk_id)?;
                chunk_hit(node_obj, chunk_obj, distance)
            })
            .collect())
    }
}

/// Drop any hit farther than [`SEMANTIC_MAX_DISTANCE`]. Preserves `hits`'
/// existing order among survivors (a plain `retain`, not a re-sort).
fn drop_hits_beyond_max_distance(mut hits: Vec<SimilarHit>) -> Vec<SimilarHit> {
    hits.retain(|hit| hit.distance <= SEMANTIC_MAX_DISTANCE);
    hits
}

/// Reciprocal rank fusion (k = 60) of two ranked hit lists, deduped by node.
/// Where a node appears in both, the semantic hit's `kind`/`path`/`snippet`
/// win (more specific — a chunk locator beats "somewhere in this node").
fn hybrid_merge(keyword_hits: Vec<SearchHit>, semantic_hits: Vec<SearchHit>, limit: usize) -> Vec<SearchHit> {
    const RRF_K: f32 = 60.0;
    let mut rrf: HashMap<NodeId, f32> = HashMap::new();
    let mut hit_by_node: HashMap<NodeId, SearchHit> = HashMap::new();

    for (rank, hit) in keyword_hits.into_iter().enumerate() {
        *rrf.entry(hit.node_id).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
        hit_by_node.entry(hit.node_id).or_insert(hit);
    }
    for (rank, hit) in semantic_hits.into_iter().enumerate() {
        *rrf.entry(hit.node_id).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
        hit_by_node.insert(hit.node_id, hit);
    }

    let mut ranked: Vec<(NodeId, f32)> = rrf.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(limit);

    ranked
        .into_iter()
        .filter_map(|(id, score)| {
            hit_by_node.remove(&id).map(|mut h| {
                h.score = score;
                h
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hit_at_distance_0_9_is_dropped() {
        let hits = vec![
            SimilarHit { object_id: 1, distance: 0.4, rerank_score: None },
            SimilarHit { object_id: 2, distance: 0.9, rerank_score: None },
        ];
        let kept = drop_hits_beyond_max_distance(hits);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].object_id, 1);
    }

    #[test]
    fn a_hit_exactly_at_the_cutoff_is_kept() {
        let hits = vec![SimilarHit {
            object_id: 1,
            distance: SEMANTIC_MAX_DISTANCE,
            rerank_score: None,
        }];
        assert_eq!(drop_hits_beyond_max_distance(hits).len(), 1);
    }

}
