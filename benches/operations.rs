use std::path::{Path, PathBuf};
use std::time::Instant;

use criterion::{BatchSize, Criterion, Throughput, black_box, criterion_group, criterion_main};
use kizu::{
    app::find_matches,
    highlight::Highlighter,
    perf,
    scar::{ScarKind, insert_scar, remove_scar},
};
use tempfile::tempdir;

fn bench_git_diff_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("git_diff_parse");
    let files = 12usize;
    let hunks_per_file = 80usize;
    let lines_per_hunk = 4usize;
    let raw = perf::large_unified_diff(files, hunks_per_file, lines_per_hunk, 96);
    group.throughput(Throughput::Elements(
        (files * hunks_per_file * lines_per_hunk) as u64,
    ));
    group.bench_function("parse_large_unified_diff", |b| {
        b.iter(|| {
            let parsed = perf::parse_unified_diff_for_bench(black_box(&raw));
            black_box(parsed.len());
        });
    });
    group.finish();
}

fn bench_layout_and_viewport(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout_and_viewport");
    group.throughput(Throughput::Elements(4_000));
    group.bench_function("build_layout_4000_hunks", |b| {
        b.iter_batched(
            || perf::many_hunk_app(4_000, 4, 96),
            |mut app| {
                perf::rebuild_layout(&mut app);
                black_box(app.layout.rows.len());
            },
            BatchSize::SmallInput,
        );
    });

    let mut app = perf::many_hunk_app(4_000, 4, 96);
    app.scroll = app.layout.hunk_starts[2_000];
    app.last_body_width.set(Some(80));
    app.wrap_lines = true;
    group.bench_function("current_hunk_range", |b| {
        b.iter(|| black_box(app.current_hunk_range()));
    });
    group.bench_function("viewport_placement_nowrap", |b| {
        b.iter(|| black_box(app.viewport_placement(black_box(32), None, Instant::now())));
    });
    group.bench_function("viewport_placement_wrap_cached", |b| {
        b.iter(|| black_box(app.viewport_placement(black_box(32), Some(80), Instant::now())));
    });
    group.finish();
}

fn bench_navigation(c: &mut Criterion) {
    let mut group = c.benchmark_group("navigation");
    group.throughput(Throughput::Elements(4_000));
    group.bench_function("next_change_1000_steps", |b| {
        b.iter_batched(
            || {
                let app = perf::many_hunk_app(4_000, 4, 96);
                app.last_body_height.set(24);
                app
            },
            |mut app| {
                for _ in 0..1_000 {
                    app.next_change();
                    let last = app.layout.rows.len().saturating_sub(1);
                    if app.scroll >= last {
                        app.scroll_to(0);
                    }
                }
                black_box(app.scroll);
            },
            BatchSize::SmallInput,
        );
    });
    group.bench_function("prev_change_1000_steps", |b| {
        b.iter_batched(
            || {
                let mut app = perf::many_hunk_app(4_000, 4, 96);
                app.last_body_height.set(24);
                let last = app.layout.rows.len().saturating_sub(1);
                app.scroll_to(last);
                app
            },
            |mut app| {
                for _ in 0..1_000 {
                    app.prev_change();
                    if app.scroll == 0 {
                        let last = app.layout.rows.len().saturating_sub(1);
                        app.scroll_to(last);
                    }
                }
                black_box(app.scroll);
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("search");
    let app = perf::many_hunk_app(4_000, 4, 96);
    group.throughput(Throughput::Elements(app.layout.rows.len() as u64));
    group.bench_function("find_matches_many_rows", |b| {
        b.iter(|| {
            let matches = find_matches(black_box(&app.layout), black_box(&app.files), "generated");
            black_box(matches.len());
        });
    });
    group.finish();
}

fn bench_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("render");
    let app = perf::many_hunk_app(800, 4, 120);
    perf::render_frame_for_bench(&app, 120, 40);
    group.bench_function("full_frame_nowrap", |b| {
        b.iter(|| perf::render_frame_for_bench(black_box(&app), 120, 40));
    });

    let mut wrapped = perf::many_hunk_app(800, 4, 180);
    wrapped.wrap_lines = true;
    perf::render_frame_for_bench(&wrapped, 120, 40);
    group.bench_function("full_frame_wrap", |b| {
        b.iter(|| perf::render_frame_for_bench(black_box(&wrapped), 120, 40));
    });
    group.finish();
}

fn bench_highlight(c: &mut Criterion) {
    let mut group = c.benchmark_group("highlight");
    let highlighter = Highlighter::new();
    let lines = perf::synthetic_source_lines(300, 96);
    let path = Path::new("bench.rs");
    group.throughput(Throughput::Elements(lines.len() as u64));
    group.bench_function("highlight_300_rust_lines", |b| {
        b.iter(|| {
            black_box(perf::highlight_lines_for_bench(
                black_box(&highlighter),
                black_box(path),
                black_box(&lines),
            ));
        });
    });
    let tsx = perf::synthetic_tsx_document(80);
    let tsx_path = Path::new("bench.tsx");
    group.throughput(Throughput::Elements(80));
    group.bench_function("highlight_80_tsx_components_document", |b| {
        b.iter(|| {
            let doc = highlighter.highlight_document(black_box(&tsx), black_box(tsx_path));
            black_box(doc.lines.len());
        });
    });
    group.finish();
}

fn bench_hook(c: &mut Criterion) {
    let mut group = c.benchmark_group("hook");
    let payload = perf::hook_payload_json(40);
    group.throughput(Throughput::Elements(40));
    group.bench_function("parse_hook_payload_40_paths", |b| {
        b.iter(|| {
            let input = perf::parse_hook_payload_for_bench(black_box(&payload));
            black_box(input.file_paths.len());
        });
    });

    let input = perf::parse_hook_payload_for_bench(&payload);
    group.bench_function("sanitize_hook_event", |b| {
        b.iter(|| {
            let event = perf::sanitize_hook_event_for_bench(black_box(&input));
            black_box(event.file_paths.len());
        });
    });

    let dir = tempdir().expect("temp scar scan dir");
    let paths = write_scan_fixture(dir.path(), 80, 120);
    group.throughput(Throughput::Elements(paths.len() as u64));
    group.bench_function("scan_scars_80_files", |b| {
        b.iter(|| {
            let hits = perf::scan_scar_files_for_bench(black_box(&paths));
            black_box(hits.len());
        });
    });
    let tsx_dir = tempdir().expect("temp tsx scar scan dir");
    let tsx_paths = write_tsx_scan_fixture(tsx_dir.path(), 80, 120);
    group.throughput(Throughput::Elements(tsx_paths.len() as u64));
    group.bench_function("scan_jsx_block_scars_80_files", |b| {
        b.iter(|| {
            let hits = perf::scan_scar_files_for_bench(black_box(&tsx_paths));
            black_box(hits.len());
        });
    });
    group.finish();
}

fn bench_scar(c: &mut Criterion) {
    let mut group = c.benchmark_group("scar");
    group.bench_function("insert_and_remove_scar_200_line_file", |b| {
        b.iter_batched(
            || {
                let dir = tempdir().expect("temp scar dir");
                let path = perf::write_scar_target(dir.path(), 200).expect("scar target");
                (dir, path)
            },
            |(_dir, path)| {
                let receipt = insert_scar(
                    black_box(&path),
                    black_box(120),
                    ScarKind::Ask,
                    "explain this change",
                )
                .expect("insert scar")
                .expect("scar inserted");
                let removed = remove_scar(&path, receipt.line_1indexed, &receipt.rendered)
                    .expect("remove scar");
                black_box(removed);
            },
            BatchSize::SmallInput,
        );
    });
    group.bench_function("insert_and_remove_scar_30_component_tsx_file", |b| {
        b.iter_batched(
            || {
                let dir = tempdir().expect("temp tsx scar dir");
                let (path, target_line) =
                    perf::write_tsx_scar_target(dir.path(), 30).expect("tsx scar target");
                (dir, path, target_line)
            },
            |(_dir, path, target_line)| {
                let receipt = insert_scar(
                    black_box(&path),
                    black_box(target_line),
                    ScarKind::Ask,
                    "explain this change",
                )
                .expect("insert tsx scar")
                .expect("scar inserted");
                let removed = remove_scar(&path, receipt.line_1indexed, &receipt.rendered)
                    .expect("remove tsx scar");
                black_box(removed);
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_stream(c: &mut Criterion) {
    let mut group = c.benchmark_group("stream");
    let previous = perf::large_unified_diff(1, 200, 2, 72);
    let current = format!("{previous}{}", perf::large_unified_diff(1, 50, 2, 72));
    group.bench_function("compute_operation_diff", |b| {
        b.iter(|| {
            let diff = perf::operation_diff_for_bench(black_box(&previous), black_box(&current));
            black_box(diff.len());
        });
    });

    let events = perf::stream_events(200, 2, 4);
    group.throughput(Throughput::Elements(events.len() as u64));
    group.bench_function("build_stream_files_200_events", |b| {
        b.iter(|| {
            let files = perf::build_stream_files_for_bench(black_box(&events));
            black_box(files.len());
        });
    });
    group.finish();
}

fn write_scan_fixture(dir: &Path, files: usize, lines: usize) -> Vec<PathBuf> {
    (0..files)
        .map(|file_idx| {
            let path = dir.join(format!("file_{file_idx}.rs"));
            let mut content = String::new();
            for line_idx in 0..lines {
                if line_idx == lines / 2 && file_idx % 8 == 0 {
                    content.push_str("// @kizu[ask]: explain this change\n");
                } else {
                    content.push_str(&format!("let value_{file_idx}_{line_idx} = {line_idx};\n"));
                }
            }
            std::fs::write(&path, content).expect("write scar scan fixture");
            path
        })
        .collect()
}

fn write_tsx_scan_fixture(dir: &Path, files: usize, lines: usize) -> Vec<PathBuf> {
    (0..files)
        .map(|file_idx| {
            let path = dir.join(format!("component_{file_idx}.tsx"));
            let mut content = String::new();
            content.push_str("export function Component() {\n  return (\n    <section>\n");
            for line_idx in 0..lines {
                if line_idx == lines / 2 && file_idx % 8 == 0 {
                    content.push_str("      {/* @kizu[ask]: explain this change */}\n");
                } else {
                    content.push_str(&format!("      <p>Item {file_idx}-{line_idx}</p>\n"));
                }
            }
            content.push_str("    </section>\n  );\n}\n");
            std::fs::write(&path, content).expect("write tsx scar scan fixture");
            path
        })
        .collect()
}

criterion_group!(
    benches,
    bench_git_diff_parse,
    bench_layout_and_viewport,
    bench_navigation,
    bench_search,
    bench_render,
    bench_highlight,
    bench_hook,
    bench_scar,
    bench_stream
);
criterion_main!(benches);
