pub fn worktree_add_argv(create: bool, reference: Option<&str>, path: &str) -> Vec<String> {
    let mut argv = vec!["worktree".to_string(), "add".to_string()];
    if create {
        argv.push("-b".to_string());
        argv.push(reference.unwrap_or_default().to_string());
    }
    argv.push(path.to_string());
    if !create {
        if let Some(reference) = reference {
            argv.push(reference.to_string());
        }
    }
    argv
}
