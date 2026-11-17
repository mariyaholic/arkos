//! CLI parser and command dispatcher for `arkos`.

use crate::config::Config;
use crate::db::{self, map_db_err};
use crate::diff::diff_snapshots;
use crate::exit::{Error, Exit, Result};
use crate::fmt::{format_delta, format_size};
use crate::logging::Logger;
use crate::paths::{PathEnvironment, Paths};
use crate::probe::{build_probe_doc, status_probe_exit};
use crate::progress::Reporter;
use crate::scan::{run_scan_with_progress, ScanMode};
use crate::top::{query_top, TopBy};
use crate::tree::write_tree_file;
use clap::{Parser, Subcommand};
use rusqlite::params;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "arkos",
    version,
    about = "Arkos volume snapshot scanner and diff"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Scan a volume or directory subtree into `shimatsu.sqlite`.
    Scan {
        #[arg(long)]
        volume: String,
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long, value_enum, default_value_t = ScanMode::Auto)]
        mode: ScanMode,
        #[arg(long = "exclude")]
        excludes: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// List stored snapshots newest first.
    List {
        #[arg(long)]
        volume: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Diff two completed snapshots by directory and file deltas.
    Diff {
        a: i64,
        b: i64,
        #[arg(long, default_value_t = 0)]
        min_size: i64,
        #[arg(long, default_value_t = 50)]
        top: usize,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Show top files or directories in a snapshot.
    Top {
        #[arg(long)]
        snapshot: i64,
        #[arg(long, value_enum, default_value_t = TopBy::Size)]
        by: TopBy,
        #[arg(long)]
        dirs: bool,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Export a static self-contained HTML + SVG treemap.
    Tree {
        #[arg(long)]
        snapshot: i64,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 4)]
        depth: i32,
    },
    /// Delete old completed snapshots while keeping the newest N per volume and root.
    Prune {
        #[arg(long)]
        keep: Option<u32>,
        #[arg(long)]
        volume: Option<String>,
        #[arg(long)]
        yes: bool,
        /// Print what would be deleted and exit 0 without deleting.
        #[arg(long, conflicts_with = "yes")]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show current scan/ledger health; supports `--probe` (exits 0/3/6).
    Status {
        #[arg(long)]
        probe: bool,
        #[arg(long)]
        json: bool,
    },
    /// Inspect or maintain `shimatsu.sqlite`.
    Db {
        #[command(subcommand)]
        action: DbAction,
    },
    /// Print resolved filesystem paths.
    Paths {
        #[arg(long)]
        json: bool,
    },
    /// Print the M0 capabilities and budgets probe JSON document.
    Probe,
}

#[derive(Debug, Subcommand)]
enum DbAction {
    Info {
        #[arg(long)]
        json: bool,
    },
    Migrate,
    Vacuum,
}

#[derive(Debug, Serialize)]
struct SnapshotSummary {
    id: i64,
    volume: String,
    root: String,
    label: Option<String>,
    mode: String,
    state: String,
    started_at: i64,
    files: i64,
    dirs: i64,
    bytes: i64,
    denied: i64,
}

/// Entrypoint returning the numeric 0..=6 exit code.
pub fn run() -> Exit {
    match try_run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("arkos: {err}");
            err.exit()
        }
    }
}

fn try_run() -> Result<Exit> {
    let cli = Cli::parse();
    if matches!(cli.command, Command::Probe) {
        let doc = build_probe_doc();
        println!(
            "{}",
            serde_json::to_string_pretty(&doc).map_err(|e| Error::Cli(e.to_string()))?
        );
        return Ok(Exit::Ok);
    }

    let env = PathEnvironment::capture();
    let paths = Paths::resolve(&env)?;
    let config = Config::load(&paths.config)?;

    if let Command::Paths { json } = cli.command {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&paths).map_err(|e| Error::Cli(e.to_string()))?
            );
        } else {
            println!("home   {}", paths.home.display());
            println!("db     {}", paths.db.display());
            println!("locks  {}", paths.locks.display());
            println!("config {}", paths.config.display());
            println!("logs   {}", paths.logs.display());
        }
        return Ok(Exit::Ok);
    }

    paths.ensure_dirs()?;
    let logger = Logger::new(&paths.logs);

    match cli.command {
        Command::Scan {
            volume,
            path,
            label,
            mode,
            excludes,
            json,
        } => {
            let mut conn = db::open_and_migrate(&paths.db, config.busy_timeout_ms)?;
            let mut progress = Reporter::auto(json);
            let report = run_scan_with_progress(
                &mut conn,
                &paths,
                &config,
                &logger,
                &volume,
                path.as_deref(),
                label.as_deref(),
                mode,
                &excludes,
                &mut progress,
            )?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|e| Error::Cli(e.to_string()))?
                );
            } else {
                println!(
                    "snapshot={} volume={} mode={} dirs={} files={} size={} scan_ms={} rollup_ms={}",
                    report.snapshot_id,
                    report.volume,
                    report.mode,
                    report.dirs,
                    report.files,
                    format_size(report.bytes.min(i64::MAX as u64) as i64),
                    report.scan_ms,
                    report.rollup_ms
                );
                if let Some(w) = report.warning {
                    println!("warning: {w}");
                }
                println!("note: {}", crate::fmt::SIZE_NOTE);
            }
            Ok(Exit::Ok)
        }
        Command::List {
            volume,
            limit,
            json,
        } => {
            let conn = db::open_and_migrate(&paths.db, config.busy_timeout_ms)?;
            let rows = list_snapshots(&conn, volume.as_deref(), limit)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&rows).map_err(|e| Error::Cli(e.to_string()))?
                );
            } else {
                for r in rows {
                    println!(
                        "#{:<4} {:<6} {:<10} {:<5} {:>10} files {:>11}  denied={}  {}",
                        r.id,
                        r.volume,
                        r.state,
                        r.mode,
                        r.files,
                        format_size(r.bytes),
                        r.denied,
                        r.root
                    );
                }
            }
            Ok(Exit::Ok)
        }
        Command::Diff {
            a,
            b,
            min_size,
            top,
            out,
            json,
        } => {
            if let Some(ref p) = out {
                if p.exists() {
                    return Err(Error::Refused(format!(
                        "output file already exists: {}",
                        p.display()
                    )));
                }
            }
            let conn = db::open_and_migrate(&paths.db, config.busy_timeout_ms)?;
            let report = diff_snapshots(&conn, a, b, min_size, top)?;
            let text =
                serde_json::to_string_pretty(&report).map_err(|e| Error::Cli(e.to_string()))?;
            if let Some(p) = out {
                std::fs::write(p, &text).map_err(|source| Error::Io {
                    context: "write diff output",
                    source,
                })?;
            } else if json {
                println!("{text}");
            } else {
                println!(
                    "diff #{} -> #{} ({})",
                    report.snapshot_a, report.snapshot_b, report.volume
                );
                for d in &report.dirs {
                    println!(
                        "dir  {:<7} {:>11}  {}",
                        format!("{:?}", d.kind),
                        format_delta(d.delta_bytes),
                        d.path
                    );
                }
                for f in &report.files {
                    println!(
                        "file {:<7} {:>11}  {}",
                        format!("{:?}", f.kind),
                        format_delta(f.delta_bytes),
                        f.path
                    );
                }
            }
            Ok(Exit::Ok)
        }
        Command::Top {
            snapshot,
            by,
            dirs,
            limit,
            json,
        } => {
            let conn = db::open_and_migrate(&paths.db, config.busy_timeout_ms)?;
            let items = query_top(&conn, snapshot, dirs, by, limit)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&items).map_err(|e| Error::Cli(e.to_string()))?
                );
            } else {
                for item in items {
                    println!("{:>11}  {}", format_size(item.bytes), item.path);
                }
            }
            Ok(Exit::Ok)
        }
        Command::Tree {
            snapshot,
            out,
            depth,
        } => {
            let conn = db::open_and_migrate(&paths.db, config.busy_timeout_ms)?;
            write_tree_file(&conn, snapshot, &out, depth)?;
            println!("wrote treemap {}", out.display());
            Ok(Exit::Ok)
        }
        Command::Prune {
            keep,
            volume,
            yes,
            dry_run,
            json,
        } => {
            let keep = keep.unwrap_or(config.snapshots_keep);
            if !(1..=12).contains(&keep) {
                return Err(Error::Cli("--keep must be between 1 and 12".into()));
            }
            let conn = db::open_and_migrate(&paths.db, config.busy_timeout_ms)?;
            if yes {
                let deleted = db::retention::prune_completed(&conn, volume.as_deref(), keep)?;
                if json {
                    println!(
                        "{}",
                        serde_json::json!({ "deleted_snapshots": deleted, "keep": keep })
                    );
                } else {
                    println!(
                        "pruned {deleted} snapshot(s), kept latest {keep} complete per volume and root"
                    );
                }
                return Ok(Exit::Ok);
            }
            let plan = db::retention::plan_prune(&conn, volume.as_deref(), keep)?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "dry_run": dry_run, "keep": keep, "candidates": plan })
                );
            } else {
                println!(
                    "would delete {} snapshot(s), keeping latest {keep} complete per volume and root",
                    plan.len()
                );
                for c in &plan {
                    println!(
                        "  #{:<4} {:<6} {:>11}  {}",
                        c.id,
                        c.volume,
                        format_size(c.bytes),
                        c.root
                    );
                }
            }
            if dry_run {
                Ok(Exit::Ok)
            } else {
                Err(Error::Cli("nothing deleted; pass --yes to delete these snapshots, or --dry-run to preview".into()))
            }
        }
        Command::Status { probe, json } => {
            let conn = db::open_and_migrate(&paths.db, config.busy_timeout_ms)?;
            let code = status_probe_exit(&conn)?;
            let info = db::info(&conn)?;
            if probe && !json {
                println!("probe={}", code.code());
            } else if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "probe_exit": code.code(),
                        "db": info,
                    }))
                    .map_err(|e| Error::Cli(e.to_string()))?
                );
            } else {
                println!(
                    "user_version={} snapshots={} dirs={} files={} probe_exit={}",
                    info.user_version,
                    info.counts.get("arkos_snapshots").copied().unwrap_or(0),
                    info.counts.get("arkos_dirs").copied().unwrap_or(0),
                    info.counts.get("arkos_files").copied().unwrap_or(0),
                    code.code(),
                );
            }
            if probe {
                Ok(code)
            } else {
                Ok(Exit::Ok)
            }
        }
        Command::Db { action } => match action {
            DbAction::Info { json } => {
                let conn = db::open_and_migrate(&paths.db, config.busy_timeout_ms)?;
                let info = db::info(&conn)?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&info)
                            .map_err(|e| Error::Cli(e.to_string()))?
                    );
                } else {
                    println!(
                        "sqlite={} user_version={} db_bytes={} wal_bytes={} snapshots={}",
                        info.sqlite_version,
                        info.user_version,
                        info.db_bytes,
                        info.wal_bytes,
                        info.counts.get("arkos_snapshots").copied().unwrap_or(0)
                    );
                }
                Ok(Exit::Ok)
            }
            DbAction::Migrate => {
                let conn = db::open_and_migrate(&paths.db, config.busy_timeout_ms)?;
                let ver: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
                println!("migrated shimatsu.sqlite user_version={ver}");
                Ok(Exit::Ok)
            }
            DbAction::Vacuum => {
                let conn = db::open_and_migrate(&paths.db, config.busy_timeout_ms)?;
                let reclaimed = db::vacuum(&conn)?;
                println!("vacuum reclaimed {reclaimed} bytes");
                Ok(Exit::Ok)
            }
        },
        Command::Paths { .. } | Command::Probe => unreachable!(),
    }
}

fn list_snapshots(
    conn: &rusqlite::Connection,
    volume: Option<&str>,
    limit: usize,
) -> Result<Vec<SnapshotSummary>> {
    let mut out = Vec::new();
    let sql = match volume {
        Some(_) => {
            "SELECT id, volume, root, label, mode, state, started_at, files, dirs, bytes, denied
             FROM arkos_snapshots WHERE volume = ?1 ORDER BY started_at DESC, id DESC LIMIT ?2"
        }
        None => {
            "SELECT id, volume, root, label, mode, state, started_at, files, dirs, bytes, denied
             FROM arkos_snapshots ORDER BY started_at DESC, id DESC LIMIT ?1"
        }
    };
    let mut stmt = conn.prepare(sql).map_err(map_db_err)?;
    let map_row = |r: &rusqlite::Row<'_>| {
        Ok(SnapshotSummary {
            id: r.get(0)?,
            volume: r.get(1)?,
            root: r.get(2)?,
            label: r.get(3)?,
            mode: r.get(4)?,
            state: r.get(5)?,
            started_at: r.get(6)?,
            files: r.get(7)?,
            dirs: r.get(8)?,
            bytes: r.get(9)?,
            denied: r.get(10)?,
        })
    };
    let rows = match volume {
        Some(v) => stmt.query_map(params![v, limit as i64], map_row),
        None => stmt.query_map(params![limit as i64], map_row),
    }
    .map_err(map_db_err)?;
    for r in rows {
        out.push(r.map_err(map_db_err)?);
    }
    Ok(out)
}
