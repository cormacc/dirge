//! /graph handler — query the entity/relation graph (#393).
//!
//! Subcommands:
//!   /graph search <query> [--kind <kind>]   FTS5 search over entities
//!   /graph traverse <id> [--depth <n>]       recursive CTE traversal

use std::path::Path;

use crate::extras::dirge_paths::ProjectPaths;
use crate::extras::session_db::SessionDb;
use crate::ui::slash::{SlashCtx, c_agent, c_error, c_result};

/// Format a timestamp for display. Trims the date portion, keeps time.
fn short_time(ts: &str) -> &str {
    ts.split_once(' ')
        .map(|(_, rest)| rest)
        .unwrap_or(ts)
}

pub(crate) async fn cmd_graph(ctx: &mut SlashCtx<'_>, parts: &[&str]) -> anyhow::Result<()> {
    let sub = parts.get(1).copied().unwrap_or("").trim();

    match sub {
        "search" => {
            let remainder = &parts[2..];
            let query = remainder
                .iter()
                .take_while(|s| **s != "--kind")
                .copied()
                .collect::<Vec<_>>()
                .join(" ");

            if query.is_empty() {
                ctx.renderer
                    .write_line("/graph search <query> [--kind <kind>]", c_agent())?;
                return Ok(());
            }

            let kind = remainder
                .iter()
                .position(|s| *s == "--kind")
                .and_then(|i| remainder.get(i + 1))
                .copied();

            let paths = ProjectPaths::new(Path::new(ctx.session.working_dir.as_str()));
            let db = match SessionDb::open(&paths.session_db_path()) {
                Ok(d) => d,
                Err(e) => {
                    ctx.renderer
                        .write_line(&format!("session db open failed: {e}"), c_error())?;
                    return Ok(());
                }
            };

            #[cfg(feature = "experimental-graph-search")]
            {
                let _ = kind;
                match crate::extras::entity_db::search_entities(&db.conn, &query, kind, 20) {
                    Ok(rows) if rows.is_empty() => {
                        ctx.renderer
                            .write_line("no entities found", c_agent())?;
                    }
                    Ok(rows) => {
                        for (id, _sid, ek, ename, extra, ts) in &rows {
                            let extra_str = extra
                                .as_deref()
                                .map(|e| format!("  {}", e))
                                .unwrap_or_default();
                            ctx.renderer.write_line(
                                &format!(
                                    "#{id}  {short}  {ek}/{ename}{extra_str}",
                                    short = short_time(ts),
                                ),
                                c_result(),
                            )?;
                        }
                        ctx.renderer.write_line(
                            &format!("{} results", rows.len()),
                            c_agent(),
                        )?;
                    }
                    Err(e) => {
                        ctx.renderer
                            .write_line(&format!("search error: {e}"), c_error())?;
                    }
                }
            }
            #[cfg(not(feature = "experimental-graph-search"))]
            {
                let _ = (db, query, kind);
                ctx.renderer.write_line(
                    "experimental-graph-search feature not enabled",
                    c_error(),
                )?;
            }
        }

        "traverse" => {
            let seed_str = parts.get(2).copied().unwrap_or("");
            let seed_id: i64 = match seed_str.parse() {
                Ok(id) => id,
                Err(_) => {
                    ctx.renderer
                        .write_line("/graph traverse <entity-id> [--depth <n>]", c_agent())?;
                    return Ok(());
                }
            };

            let depth: u32 = parts
                .iter()
                .position(|s| *s == "--depth")
                .and_then(|i| parts.get(i + 1))
                .and_then(|s| s.parse().ok())
                .unwrap_or(3);

            let paths = ProjectPaths::new(Path::new(ctx.session.working_dir.as_str()));
            let db = match SessionDb::open(&paths.session_db_path()) {
                Ok(d) => d,
                Err(e) => {
                    ctx.renderer
                        .write_line(&format!("session db open failed: {e}"), c_error())?;
                    return Ok(());
                }
            };

            #[cfg(feature = "experimental-graph-search")]
            {
                let _ = depth;
                match crate::extras::entity_search::traverse_from(&db.conn, &[seed_id], depth) {
                    Ok(rows) if rows.is_empty() => {
                        ctx.renderer.write_line(
                            &format!("entity #{seed_id} not found or no edges"),
                            c_agent(),
                        )?;
                    }
                    Ok(rows) => {
                        for (_id, path, d) in &rows {
                            ctx.renderer.write_line(
                                &format!("  d={d}  {path}"),
                                c_result(),
                            )?;
                        }
                        ctx.renderer.write_line(
                            &format!("{} nodes", rows.len()),
                            c_agent(),
                        )?;
                    }
                    Err(e) => {
                        ctx.renderer
                            .write_line(&format!("traverse error: {e}"), c_error())?;
                    }
                }
            }
            #[cfg(not(feature = "experimental-graph-search"))]
            {
                let _ = (db, seed_id, depth);
                ctx.renderer.write_line(
                    "experimental-graph-search feature not enabled",
                    c_error(),
                )?;
            }
        }

        "" => {
            ctx.renderer.write_line(
                "/graph search <query> [--kind <kind>]   FTS5 search over entities",
                c_agent(),
            )?;
            ctx.renderer.write_line(
                "/graph traverse <id> [--depth <n>]       recursive CTE traversal from entity",
                c_agent(),
            )?;
            ctx.renderer.write_line("", c_agent())?;
            ctx.renderer.write_line("requires experimental-graph-search feature", c_agent())?;
        }

        other => {
            ctx.renderer
                .write_line(&format!("unknown /graph sub-command: {other}"), c_error())?;
        }
    }

    Ok(())
}
