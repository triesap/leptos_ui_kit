use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=LEPTOS_UI_KIT_GIT_REV");

    if let Ok(rev) = std::env::var("LEPTOS_UI_KIT_GIT_REV") {
        let rev = rev.trim();
        if is_git_rev(rev) {
            println!("cargo:rustc-env=LEPTOS_UI_KIT_GIT_REV={rev}");
            return;
        }
    }

    let Ok(output) = Command::new("git").args(["rev-parse", "HEAD"]).output() else {
        return;
    };

    if !output.status.success() {
        return;
    }

    let Ok(rev) = String::from_utf8(output.stdout) else {
        return;
    };
    let rev = rev.trim();
    if is_git_rev(rev) {
        println!("cargo:rustc-env=LEPTOS_UI_KIT_GIT_REV={rev}");
    }
}

fn is_git_rev(rev: &str) -> bool {
    rev.len() == 40 && rev.bytes().all(|byte| byte.is_ascii_hexdigit())
}
