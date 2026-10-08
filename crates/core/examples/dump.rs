use am_inspect::columns::{build_rows, decode_runs};

fn main() {
    let path = std::env::args().nth(1).expect("path");
    let bytes = std::fs::read(&path).unwrap();
    let report = am_inspect::parse_file(&bytes);
    for c in report.chunks.iter().take(3) {
        println!("#{} {} {}B changes={} ops={} actors={}", c.index, c.kind.label(), c.len(), c.num_changes(), c.num_ops(), c.facts.actors.len());
        for s in &c.sections {
            println!("   {:<24} {:>8}", s.label, s.len());
        }
        for g in &c.groups {
            println!("   {} rows={} stored={} inflated={}", g.label, g.rows, g.stored_len(), g.inflated_len());
            for col in &g.columns {
                let r = decode_runs(col);
                println!("     {:<18} {:<6} {:>7}B {:>7}B{} runs={} rows={}", col.name, col.col_type().short(), col.stored_len, col.data.len(), if col.deflated() {" z"} else {""}, r.stats.runs, r.stats.rows);
            }
            let t = build_rows(g);
            for row in 0..t.rows.min(4) {
                let cells: Vec<String> = t.fields.iter().map(|f| format!("{}={}", f.name, f.render(row))).collect();
                println!("       {}", cells.join("  "));
            }
        }
    }
}
