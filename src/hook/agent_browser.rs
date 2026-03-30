use std::process::{Command, Stdio};

pub fn close_all_sessions() {
    let (bin, prefix) = find_pw();
    let mut args = prefix.clone();
    args.extend(["session".into(), "list".into()]);
    let out = Command::new(&bin)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    if let Ok(o) = out {
        let text = String::from_utf8_lossy(&o.stdout);
        for line in text.lines().skip(1) {
            if let Some(id) = line.split_whitespace().next() {
                if id.parse::<u32>().is_ok() {
                    let mut reset_args = prefix.clone();
                    reset_args.extend(["session".into(), "reset".into(), id.to_string()]);
                    let _ = Command::new(&bin).args(&reset_args).output();
                }
            }
        }
    }
}

fn find_pw() -> (String, Vec<String>) {
    let npm_dir = if cfg!(windows) {
        std::env::var("APPDATA").map(|d| std::path::PathBuf::from(d).join("npm")).ok()
    } else {
        None
    };
    if let Some(ref dir) = npm_dir {
        let bin_js = dir.join("node_modules").join("playwriter").join("bin.js");
        if bin_js.exists() {
            return ("node".to_string(), vec![bin_js.to_string_lossy().to_string()]);
        }
    }
    ("playwriter".to_string(), vec![])
}
