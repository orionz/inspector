pub fn bytes(n: usize) -> String {
    let n = n as f64;
    if n < 1000.0 {
        format!("{n} B")
    } else if n < 1_000_000.0 {
        format!("{:.1} KB", n / 1000.0)
    } else {
        format!("{:.2} MB", n / 1_000_000.0)
    }
}

pub fn count(n: usize) -> String {
    if n < 10_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{:.2}M", n as f64 / 1_000_000.0)
    }
}

pub fn pct(part: usize, whole: usize) -> String {
    if whole == 0 {
        return "–".into();
    }
    let p = part as f64 * 100.0 / whole as f64;
    if p > 0.0 && p < 0.1 {
        "<0.1%".into()
    } else {
        format!("{p:.1}%")
    }
}

pub fn ratio(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

pub fn per_row(bytes: usize, rows: usize) -> String {
    if rows == 0 {
        "–".into()
    } else {
        format!("{:.2}", bytes as f64 / rows as f64)
    }
}

pub fn short(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub fn hex_preview(b: &[u8], max: usize) -> String {
    let mut s: String = b
        .iter()
        .take(max)
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    if b.len() > max {
        s.push_str(" …");
    }
    s
}

/// `n` with thousands separators, e.g. `12,345`.
pub fn count_exact(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}
