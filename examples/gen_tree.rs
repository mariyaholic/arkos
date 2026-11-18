//! Fixture generator: `gen_tree --files N --dirs M --depth D --seed S --out PATH`.
//!
//! Writes a deterministic sparse tree and prints a manifest hash over the sorted
//! `(path, size)` list, so measurements can name the exact tree they ran on. The tree logic is
//! shared with the integration tests through `tests/support/fixture.rs`.

#[path = "../tests/support/fixture.rs"]
mod fixture;

use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage: gen_tree --files N --dirs M --depth D [--seed S] --out PATH");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut files = None;
    let mut dirs = None;
    let mut depth = None;
    let mut seed = 1u64;
    let mut out: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let Some(value) = args.next() else {
            return usage();
        };
        match flag.as_str() {
            "--files" => files = value.parse::<usize>().ok(),
            "--dirs" => dirs = value.parse::<usize>().ok(),
            "--depth" => depth = value.parse::<usize>().ok(),
            "--seed" => match value.parse::<u64>() {
                Ok(v) => seed = v,
                Err(_) => return usage(),
            },
            "--out" => out = Some(PathBuf::from(value)),
            _ => return usage(),
        }
    }
    let (Some(files), Some(dirs), Some(depth), Some(out)) = (files, dirs, depth, out) else {
        return usage();
    };

    if let Err(err) = std::fs::create_dir_all(&out) {
        eprintln!("gen_tree: cannot create {}: {err}", out.display());
        return ExitCode::from(1);
    }
    let spec = fixture::Spec {
        files,
        dirs,
        depth,
        seed,
    };
    match fixture::generate(&out, &spec) {
        Ok(m) => {
            println!(
                "files={} dirs={} bytes={} hash={:016x}",
                m.files, m.dirs, m.bytes, m.hash
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("gen_tree: {err}");
            ExitCode::from(1)
        }
    }
}
