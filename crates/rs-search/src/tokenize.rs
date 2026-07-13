use std::collections::HashSet;

pub(crate) fn split_camel(word: &str) -> Vec<String> {
    let chars: Vec<char> = word.chars().collect();
    let mut tokens = Vec::new();
    let mut cur = String::new();
    for i in 0..chars.len() {
        let c = chars[i];
        let is_upper = c.is_uppercase();
        let prev_lower = i > 0 && chars[i-1].is_lowercase();
        let next_lower = i + 1 < chars.len() && chars[i+1].is_lowercase();
        if is_upper && (prev_lower || next_lower) && !cur.is_empty() {
            tokens.push(cur.to_lowercase());
            cur = c.to_string();
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() { tokens.push(cur.to_lowercase()); }
    tokens
}

pub fn add_word_tokens(word: &str, out: &mut HashSet<String>) {
    if word.chars().any(|c| c.is_uppercase()) {
        for t in split_camel(word) {
            for p in t.split(|c: char| !c.is_alphanumeric()) {
                if !p.is_empty() { out.insert(p.to_string()); }
            }
        }
    }
    for part in word.split(|c: char| !c.is_alphanumeric()) {
        let pc = part.to_lowercase();
        if !pc.is_empty() { out.insert(pc); }
    }
    if word.chars().all(|c| c.is_alphanumeric() || c == '_') {
        let cleaned = word.to_lowercase();
        if !cleaned.is_empty() { out.insert(cleaned); }
    }
}

pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens: HashSet<String> = HashSet::new();
    for word in text.split_whitespace() { add_word_tokens(word, &mut tokens); }
    let mut v: Vec<String> = tokens.into_iter().collect();
    v.sort();
    v
}
