---
key: mem-9ab78e1dd45e2daf-482
ns: default
created: 1789417766479
updated: 1789417766479
---

plugkit/embed-two-bert-routes: host_vec_embed (agentplug-host imports.rs) returns -1 for every failure (bert sibling absent, capability denied, cold or evicted pool slot, model error) after 3 attempts 500 ms apart, and logs nothing when bert is absent. embed::try_sibling_plugin_embed reaches the same bert model through host_plugin_call (registry pool path) and returns unknown_plugin / plugin_not_loaded_yet / the plugin's own error, so it is a real second route, not a duplicate.
