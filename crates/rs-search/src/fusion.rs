use std::collections::HashMap;

pub const RRF_K: f64 = 60.0;
pub const IDENTIFIER_BOOST: f64 = 1.5;

pub(crate) fn looks_like_identifier(query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() || q.contains(' ') { return false; }
    let has_non_digit_non_period = q.chars().any(|c| c != '.' && !c.is_ascii_digit());
    let has_separator = has_non_digit_non_period
        && (q.contains('_') || q.contains('-') || q.contains('.'));
    let chars: Vec<char> = q.chars().collect();
    let has_mid_word_case_transition = chars.windows(2).enumerate().any(|(i, w)| {
        if i == 0 { return false; }
        (w[0].is_lowercase() && w[1].is_uppercase()) || (w[0].is_uppercase() && w[1].is_lowercase())
    });
    has_separator || has_mid_word_case_transition
}

pub(crate) fn rrf_merge_n(ranked_lists: &[Vec<String>]) -> Vec<(String, f64)> {
    rrf_merge_n_weighted(ranked_lists, &[])
}

pub(crate) fn rrf_merge_n_weighted(ranked_lists: &[Vec<String>], weights: &[f64]) -> Vec<(String, f64)> {
    let mut scores: HashMap<String, f64> = HashMap::new();
    for (li, ranked) in ranked_lists.iter().enumerate() {
        let w = weights.get(li).copied().unwrap_or(1.0);
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (rank, id) in ranked.iter().enumerate() {
            if !seen.insert(id.as_str()) { continue; }
            *scores.entry(id.clone()).or_insert(0.0) += w / (RRF_K + (rank + 1) as f64);
        }
    }
    let mut out: Vec<(String, f64)> = scores.into_iter().collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(&b.0)));
    out
}

pub fn fuse_n(ranked_lists: &[Vec<String>], weights: &[f64], query: &str) -> Vec<(String, f64)> {
    if looks_like_identifier(query) {
        rrf_merge_n_weighted(ranked_lists, weights)
    } else {
        rrf_merge_n(ranked_lists)
    }
}
