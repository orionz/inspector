use std::fmt;
use std::sync::Arc;

use crate::leb::{read_sleb, read_uleb, ParseError, Reader, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColType {
    Group,
    Actor,
    Integer,
    DeltaInteger,
    Boolean,
    String,
    ValueMetadata,
    Value,
}

impl ColType {
    fn from_bits(b: u32) -> Self {
        match b & 0b111 {
            0 => Self::Group,
            1 => Self::Actor,
            2 => Self::Integer,
            3 => Self::DeltaInteger,
            4 => Self::Boolean,
            5 => Self::String,
            6 => Self::ValueMetadata,
            _ => Self::Value,
        }
    }

    pub fn short(&self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Actor => "actor",
            Self::Integer => "uint",
            Self::DeltaInteger => "delta",
            Self::Boolean => "bool",
            Self::String => "str",
            Self::ValueMetadata => "vmeta",
            Self::Value => "raw",
        }
    }
}

impl fmt::Display for ColType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.short())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnSpec(pub u32);

impl ColumnSpec {
    pub fn id(&self) -> u32 {
        self.0 >> 4
    }
    pub fn col_type(&self) -> ColType {
        ColType::from_bits(self.0)
    }
    pub fn deflate(&self) -> bool {
        self.0 & 0b1000 != 0
    }
}

/// Which table of column names applies; the same column id means different
/// things in different chunk sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColContext {
    DocOps,
    DocChanges,
    ChangeOps,
    BundleOps,
    BundleChanges,
    BundleV0Changes,
}

impl ColContext {
    pub fn is_ops(&self) -> bool {
        matches!(self, Self::DocOps | Self::ChangeOps | Self::BundleOps)
    }

    /// The base name of the logical field a column id belongs to.
    pub fn field_name(&self, id: u32) -> Option<&'static str> {
        use ColContext::*;
        Some(match (self, id) {
            (DocOps | ChangeOps | BundleOps, 0) => "obj",
            (DocOps | ChangeOps | BundleOps, 1) => "key",
            (DocOps | BundleOps, 2) => "id",
            (DocOps | ChangeOps | BundleOps, 3) => "insert",
            (DocOps | ChangeOps | BundleOps, 4) => "action",
            (DocOps | ChangeOps | BundleOps, 5) => "value",
            (ChangeOps | BundleOps, 7) => "pred",
            (DocOps | BundleOps, 8) => "succ",
            (DocOps | ChangeOps | BundleOps, 9) => "expand",
            (DocOps | ChangeOps | BundleOps, 10) => "mark_name",
            (BundleOps, 11) => "id_ctr_inverse",
            (BundleOps, 12) => "hint",
            (DocChanges, 0) | (BundleChanges | BundleV0Changes, 0) => "change",
            (DocChanges, 1) => "max_op",
            (DocChanges, 2) => "time",
            (DocChanges, 3) => "message",
            (DocChanges, 4) => "deps",
            (DocChanges, 5) => "extra",
            (DocChanges, 6) => "hash (legacy)",
            (BundleChanges, 1) => "num_ops",
            (BundleV0Changes, 1) => "start_op",
            (BundleChanges | BundleV0Changes, 2) => "max_op",
            (BundleChanges | BundleV0Changes, 3) => "time",
            (BundleChanges | BundleV0Changes, 4) => "message",
            (BundleChanges | BundleV0Changes, 5) => "deps",
            (BundleChanges | BundleV0Changes, 6) => "extra",
            _ => return None,
        })
    }

    pub fn column_name(&self, spec: ColumnSpec) -> String {
        let id = spec.id();
        let ty = spec.col_type();
        let Some(base) = self.field_name(id) else {
            return format!("col{id}.{ty}");
        };
        let is_change_id = base == "change";
        let suffix = match ty {
            ColType::Actor => "actor",
            ColType::Integer | ColType::DeltaInteger if is_change_id => "seq",
            ColType::Integer | ColType::DeltaInteger
                if matches!(base, "obj" | "key" | "id" | "pred" | "succ") =>
            {
                "ctr"
            }
            ColType::String if base == "key" => "str",
            ColType::Group => "count",
            ColType::ValueMetadata => "meta",
            ColType::Value => "raw",
            _ => "",
        };
        if suffix.is_empty() {
            base.to_string()
        } else {
            format!("{base}.{suffix}")
        }
    }
}

#[derive(Debug, Clone)]
pub struct ColumnInfo {
    pub spec: ColumnSpec,
    pub name: String,
    /// Absolute file offset of the stored bytes, when they appear verbatim in the file.
    pub file_offset: Option<usize>,
    pub stored_len: usize,
    /// Column bytes after inflating, which is what runs and rows are decoded from.
    pub data: Arc<[u8]>,
}

impl ColumnInfo {
    pub fn col_type(&self) -> ColType {
        self.spec.col_type()
    }
    pub fn deflated(&self) -> bool {
        self.spec.deflate()
    }
}

#[derive(Debug, Clone)]
pub struct ColumnGroup {
    pub label: &'static str,
    pub context: ColContext,
    pub columns: Vec<ColumnInfo>,
    /// Bytes for the column spec/length table.
    pub meta_len: usize,
    pub rows: usize,
}

impl ColumnGroup {
    pub fn stored_len(&self) -> usize {
        self.columns.iter().map(|c| c.stored_len).sum()
    }

    pub fn inflated_len(&self) -> usize {
        self.columns.iter().map(|c| c.data.len()).sum()
    }

    pub fn new(
        label: &'static str,
        context: ColContext,
        columns: Vec<ColumnInfo>,
        meta_len: usize,
    ) -> Self {
        let rows = columns.iter().find_map(column_len).unwrap_or(0);
        Self {
            label,
            context,
            columns,
            meta_len,
            rows,
        }
    }
}

/// Parsed (spec, length) table that precedes a block of column data.
pub fn parse_column_meta(r: &mut Reader<'_>) -> Result<Vec<(ColumnSpec, usize)>> {
    let n = r.count(2)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let spec = r.uleb()?;
        let len = r.uleb()?;
        out.push((ColumnSpec(spec as u32), len as usize));
    }
    Ok(out)
}

/// Splits `data` (starting at absolute `offset`) into columns, inflating deflated ones.
pub fn build_columns(
    context: ColContext,
    meta: &[(ColumnSpec, usize)],
    data: &[u8],
    offset: Option<usize>,
) -> Result<Vec<ColumnInfo>> {
    let mut pos = 0;
    let mut out = Vec::with_capacity(meta.len());
    for (spec, len) in meta {
        let end = pos + len;
        if end > data.len() {
            return Err(ParseError {
                offset: offset.unwrap_or(0) + pos,
                what: format!("column {} overruns its data block", context.column_name(*spec)),
            });
        }
        let stored = &data[pos..end];
        let bytes: Arc<[u8]> = if spec.deflate() {
            inflate(stored)
                .map_err(|e| ParseError {
                    offset: offset.unwrap_or(0) + pos,
                    what: format!("inflating column: {e}"),
                })?
                .into()
        } else {
            stored.into()
        };
        out.push(ColumnInfo {
            spec: *spec,
            name: context.column_name(*spec),
            file_offset: offset.map(|o| o + pos),
            stored_len: *len,
            data: bytes,
        });
        pos = end;
    }
    Ok(out)
}

pub fn inflate(data: &[u8]) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(data).read_to_end(&mut out)?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Runs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Repeat,
    Literal,
    Null,
    Bool,
    Raw,
}

impl RunKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Repeat => "repeat",
            Self::Literal => "literal",
            Self::Null => "null",
            Self::Bool => "bool",
            Self::Raw => "raw",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    U(u64),
    I(i64),
    B(bool),
    S(Arc<str>),
}

impl fmt::Display for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cell::Null => f.write_str("null"),
            Cell::U(v) => write!(f, "{v}"),
            Cell::I(v) => write!(f, "{v}"),
            Cell::B(v) => write!(f, "{v}"),
            Cell::S(s) => write!(f, "{s:?}"),
        }
    }
}

impl Cell {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Cell::U(v) => Some(*v),
            Cell::I(v) if *v >= 0 => Some(*v as u64),
            _ => None,
        }
    }
}

/// One encoded run within a column.
#[derive(Debug, Clone)]
pub struct Run {
    pub kind: RunKind,
    /// Number of rows the run covers.
    pub count: usize,
    pub first_row: usize,
    /// Byte range within the (inflated) column.
    pub bytes: std::ops::Range<usize>,
    /// The raw values as stored (deltas for delta columns).
    pub values: Vec<Cell>,
    /// For literal runs, where each value starts within the column.
    pub value_starts: Vec<usize>,
    /// Absolute values of the first and last row for delta columns.
    pub delta_span: Option<(i64, i64)>,
}

#[derive(Debug, Clone, Default)]
pub struct RunStats {
    pub rows: usize,
    pub runs: usize,
    pub repeat_rows: usize,
    pub repeat_bytes: usize,
    pub literal_rows: usize,
    pub literal_bytes: usize,
    pub null_rows: usize,
    pub null_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct DecodedRuns {
    pub runs: Vec<Run>,
    pub stats: RunStats,
    pub error: Option<String>,
}

fn rle_value(ty: ColType, b: &[u8]) -> Option<(Cell, usize)> {
    match ty {
        ColType::DeltaInteger => read_sleb(b).map(|(v, n)| (Cell::I(v), n)),
        ColType::String => {
            let (len, n) = read_uleb(b)?;
            let end = n.checked_add(len as usize)?;
            let s = b.get(n..end)?;
            Some((Cell::S(String::from_utf8_lossy(s).into()), end))
        }
        _ => read_uleb(b).map(|(v, n)| (Cell::U(v), n)),
    }
}

/// Decodes a column into its runs. Value columns need the matching metadata
/// column to be split into rows, so they're reported as a single raw run.
pub fn decode_runs(col: &ColumnInfo) -> DecodedRuns {
    let ty = col.col_type();
    let data = &col.data[..];
    let mut runs = Vec::new();
    let mut stats = RunStats::default();
    let mut error = None;
    match ty {
        ColType::Value => {
            runs.push(Run {
                kind: RunKind::Raw,
                count: 0,
                first_row: 0,
                bytes: 0..data.len(),
                values: vec![],
                value_starts: vec![],
                delta_span: None,
            });
        }
        ColType::Boolean => {
            let mut pos = 0;
            let mut value = false;
            let mut row = 0;
            while pos < data.len() {
                let Some((count, n)) = read_uleb(&data[pos..]) else {
                    error = Some(format!("bad LEB128 at column byte {pos}"));
                    break;
                };
                if count > 0 {
                    runs.push(Run {
                        kind: RunKind::Bool,
                        count: count as usize,
                        first_row: row,
                        bytes: pos..pos + n,
                        values: vec![Cell::B(value)],
                        value_starts: vec![],
                        delta_span: None,
                    });
                    stats.repeat_rows += count as usize;
                    stats.repeat_bytes += n;
                }
                row += count as usize;
                pos += n;
                value = !value;
            }
            stats.rows = row;
        }
        _ => {
            let mut pos = 0;
            let mut row = 0;
            let mut abs: i64 = 0;
            while pos < data.len() {
                let start = pos;
                let Some((count, n)) = read_sleb(&data[pos..]) else {
                    error = Some(format!("bad run header at column byte {pos}"));
                    break;
                };
                pos += n;
                let mut value_starts = Vec::new();
                let (kind, rows, values) = if count > 0 {
                    let Some((v, n)) = rle_value(ty, &data[pos..]) else {
                        error = Some(format!("bad value at column byte {pos}"));
                        break;
                    };
                    pos += n;
                    (RunKind::Repeat, count as usize, vec![v])
                } else if count < 0 {
                    let k = count.unsigned_abs() as usize;
                    let mut vals = Vec::with_capacity(k.min(4096));
                    value_starts.reserve(k.min(4096));
                    for _ in 0..k {
                        value_starts.push(pos);
                        let Some((v, n)) = rle_value(ty, &data[pos..]) else {
                            error = Some(format!("bad value at column byte {pos}"));
                            break;
                        };
                        pos += n;
                        vals.push(v);
                    }
                    if error.is_some() {
                        break;
                    }
                    (RunKind::Literal, k, vals)
                } else {
                    let Some((nulls, n)) = read_uleb(&data[pos..]) else {
                        error = Some(format!("bad null count at column byte {pos}"));
                        break;
                    };
                    pos += n;
                    (RunKind::Null, nulls as usize, vec![])
                };
                let delta_span = if ty == ColType::DeltaInteger {
                    let first = match (&kind, values.first()) {
                        (RunKind::Null, _) => abs,
                        (_, Some(Cell::I(d))) => abs.wrapping_add(*d),
                        _ => abs,
                    };
                    match kind {
                        RunKind::Repeat => {
                            if let Some(Cell::I(d)) = values.first() {
                                abs = abs.wrapping_add(d.wrapping_mul(rows as i64));
                            }
                        }
                        RunKind::Literal => {
                            for v in &values {
                                if let Cell::I(d) = v {
                                    abs = abs.wrapping_add(*d);
                                }
                            }
                        }
                        _ => {}
                    }
                    Some((first, abs))
                } else {
                    None
                };
                let len = pos - start;
                match kind {
                    RunKind::Repeat => {
                        stats.repeat_rows += rows;
                        stats.repeat_bytes += len;
                    }
                    RunKind::Literal => {
                        stats.literal_rows += rows;
                        stats.literal_bytes += len;
                    }
                    _ => {
                        stats.null_rows += rows;
                        stats.null_bytes += len;
                    }
                }
                runs.push(Run {
                    kind,
                    count: rows,
                    first_row: row,
                    bytes: start..pos,
                    values,
                    value_starts,
                    delta_span,
                });
                row += rows;
            }
            stats.rows = row;
        }
    }
    stats.runs = runs.len();
    DecodedRuns {
        runs,
        stats,
        error,
    }
}

/// Number of rows encoded in a column (value columns have no intrinsic row count).
pub fn column_len(col: &ColumnInfo) -> Option<usize> {
    if col.col_type() == ColType::Value {
        return None;
    }
    let d = decode_runs(col);
    Some(d.stats.rows)
}

/// Every row of an RLE/bool/delta column; deltas are resolved to absolute values.
pub fn decode_cells(col: &ColumnInfo) -> Vec<Cell> {
    let d = decode_runs(col);
    let mut out = Vec::with_capacity(d.stats.rows);
    let delta = col.col_type() == ColType::DeltaInteger;
    let mut abs: i64 = 0;
    for run in &d.runs {
        match run.kind {
            RunKind::Null => out.extend(std::iter::repeat_n(Cell::Null, run.count)),
            RunKind::Raw => {}
            RunKind::Bool | RunKind::Repeat if !delta => {
                out.extend(std::iter::repeat_n(run.values[0].clone(), run.count))
            }
            RunKind::Repeat => {
                let Cell::I(d) = run.values[0] else { continue };
                for _ in 0..run.count {
                    abs = abs.wrapping_add(d);
                    out.push(Cell::I(abs));
                }
            }
            RunKind::Literal if delta => {
                for v in &run.values {
                    if let Cell::I(d) = v {
                        abs = abs.wrapping_add(*d);
                        out.push(Cell::I(abs));
                    }
                }
            }
            _ => out.extend(run.values.iter().cloned()),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Values

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Uint(u64),
    Int(i64),
    F64(f64),
    Str(String),
    Bytes(Vec<u8>),
    Counter(i64),
    Timestamp(i64),
    Unknown(u8, Vec<u8>),
    Invalid(String),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => f.write_str("null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Uint(v) => write!(f, "{v}u"),
            Value::Int(v) => write!(f, "{v}"),
            Value::F64(v) => write!(f, "{v}f"),
            Value::Str(s) => write!(f, "{:?}", truncate(s, 80)),
            Value::Bytes(b) => write!(f, "bytes[{}]", b.len()),
            Value::Counter(v) => write!(f, "counter({v})"),
            Value::Timestamp(v) => write!(f, "ts({v})"),
            Value::Unknown(t, b) => write!(f, "unknown(type {t}, {} bytes)", b.len()),
            Value::Invalid(e) => write!(f, "<{e}>"),
        }
    }
}

/// Bytes as a slice of hex, e.g. `[]` or `[de ad]`.
pub fn slice_hex(b: &[u8]) -> String {
    let hex: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("[{}]", hex.join(" "))
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

pub fn value_type_name(code: u64) -> &'static str {
    match code {
        0 => "null",
        1 => "false",
        2 => "true",
        3 => "uint",
        4 => "int",
        5 => "f64",
        6 => "str",
        7 => "bytes",
        8 => "counter",
        9 => "timestamp",
        _ => "unknown",
    }
}

pub fn decode_value(code: u64, raw: &[u8]) -> Value {
    let leb_u = || read_uleb(raw).map(|(v, _)| v);
    let leb_s = || read_sleb(raw).map(|(v, _)| v);
    match code {
        0 => Value::Null,
        1 => Value::Bool(false),
        2 => Value::Bool(true),
        3 => leb_u().map(Value::Uint).unwrap_or(Value::Invalid("bad uint".into())),
        4 => leb_s().map(Value::Int).unwrap_or(Value::Invalid("bad int".into())),
        5 => raw
            .try_into()
            .map(|b: [u8; 8]| Value::F64(f64::from_le_bytes(b)))
            .unwrap_or(Value::Invalid("bad f64".into())),
        6 => Value::Str(String::from_utf8_lossy(raw).into_owned()),
        7 => Value::Bytes(raw.to_vec()),
        8 => leb_s().map(Value::Counter).unwrap_or(Value::Invalid("bad counter".into())),
        9 => leb_s().map(Value::Timestamp).unwrap_or(Value::Invalid("bad timestamp".into())),
        t => Value::Unknown(t as u8, raw.to_vec()),
    }
}

// ---------------------------------------------------------------------------
// Rows: columns recombined into the logical fields they encode

#[derive(Debug, Clone)]
pub enum FieldData {
    Scalar(Vec<Cell>),
    /// `counter@actor`, e.g. op ids and change ids.
    Id {
        actor: Vec<Cell>,
        ctr: Vec<Cell>,
    },
    /// A map key string or an element id.
    Key {
        actor: Vec<Cell>,
        ctr: Vec<Cell>,
        string: Vec<Cell>,
    },
    Value {
        meta: Vec<Cell>,
        /// Start offset of each row's value bytes within `raw`.
        offsets: Vec<usize>,
        raw: Arc<[u8]>,
    },
    Group {
        counts: Vec<Cell>,
        /// Index of each row's first item within the sub-fields.
        starts: Vec<usize>,
        items: Vec<Field>,
    },
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub data: FieldData,
}

#[derive(Debug, Clone)]
pub struct RowTable {
    pub fields: Vec<Field>,
    pub rows: usize,
}

fn value_field(meta: Option<&ColumnInfo>, raw: Option<&ColumnInfo>) -> FieldData {
    let meta = meta.map(decode_cells).unwrap_or_default();
    let mut offsets = Vec::with_capacity(meta.len() + 1);
    let mut pos = 0usize;
    for m in &meta {
        offsets.push(pos);
        pos += (m.as_u64().unwrap_or(0) >> 4) as usize;
    }
    offsets.push(pos);
    FieldData::Value {
        meta,
        offsets,
        raw: raw.map(|c| c.data.clone()).unwrap_or_else(|| Arc::from(&[][..])),
    }
}

fn field_from_cols(context: ColContext, id: u32, cols: &[&ColumnInfo]) -> Field {
    let find = |t: ColType| cols.iter().copied().find(|c| c.col_type() == t);
    let name = context
        .field_name(id)
        .map(str::to_string)
        .unwrap_or_else(|| format!("col{id}"));
    let actor = find(ColType::Actor);
    let ctr = find(ColType::DeltaInteger).or(find(ColType::Integer));
    let string = find(ColType::String);
    let data = if find(ColType::ValueMetadata).is_some() || find(ColType::Value).is_some() {
        value_field(find(ColType::ValueMetadata), find(ColType::Value))
    } else if let (Some(a), Some(c)) = (actor, ctr) {
        if string.is_some() {
            FieldData::Key {
                actor: decode_cells(a),
                ctr: decode_cells(c),
                string: string.map(decode_cells).unwrap_or_default(),
            }
        } else {
            FieldData::Id {
                actor: decode_cells(a),
                ctr: decode_cells(c),
            }
        }
    } else if let (Some(c), Some(s)) = (ctr, string) {
        FieldData::Key {
            actor: vec![],
            ctr: decode_cells(c),
            string: decode_cells(s),
        }
    } else {
        let col = cols[0];
        FieldData::Scalar(decode_cells(col))
    };
    Field { name, data }
}

/// Reassembles a column group into logical rows.
pub fn build_rows(group: &ColumnGroup) -> RowTable {
    let mut fields = Vec::new();
    let mut i = 0;
    let cols = &group.columns;
    while i < cols.len() {
        let id = cols[i].spec.id();
        let mut j = i;
        while j < cols.len() && cols[j].spec.id() == id {
            j += 1;
        }
        let same: Vec<&ColumnInfo> = cols[i..j].iter().collect();
        if let Some(g) = same.iter().find(|c| c.col_type() == ColType::Group) {
            let counts = decode_cells(g);
            let mut starts = Vec::with_capacity(counts.len() + 1);
            let mut total = 0;
            for c in &counts {
                starts.push(total);
                total += c.as_u64().unwrap_or(0) as usize;
            }
            starts.push(total);
            let members: Vec<&ColumnInfo> = same
                .iter()
                .copied()
                .filter(|c| c.col_type() != ColType::Group)
                .collect();
            let base = group
                .context
                .field_name(id)
                .map(str::to_string)
                .unwrap_or_else(|| format!("col{id}"));
            let item = if members.is_empty() {
                vec![]
            } else {
                let mut f = field_from_cols(group.context, id, &members);
                f.name = format!("{base}[]");
                vec![f]
            };
            fields.push(Field {
                name: base,
                data: FieldData::Group {
                    counts,
                    starts,
                    items: item,
                },
            });
        } else {
            fields.push(field_from_cols(group.context, id, &same));
        }
        i = j;
    }
    let rows = fields.iter().map(|f| f.len()).max().unwrap_or(0);
    RowTable { fields, rows }
}

pub const ACTIONS: [&str; 8] = [
    "makeMap", "set", "makeList", "del", "makeText", "inc", "makeTable", "mark",
];

/// An op's action as the inspector shows it: `Make Map`, `Set 5`, `Del`,
/// `Inc 2`, with `Insert` in place of `Make`/`Set` for inserted list elements.
pub fn action_label(action: &str, insert: bool, value: Option<&str>) -> String {
    let with = |name: &str| match value {
        Some(v) => format!("{name} {v}"),
        None => name.to_string(),
    };
    match action.strip_prefix("make") {
        Some(ty) if insert => format!("Insert {ty}"),
        Some(ty) => format!("Make {ty}"),
        None => match action {
            "set" if insert => with("Insert"),
            "del" => "Del".into(),
            a => with(&(a[..1].to_uppercase() + &a[1..])),
        },
    }
}

/// A column of the logical view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShownCol {
    Field(usize),
    /// An op's action with its value and insert flag, e.g. `Set "cow"`,
    /// `Insert "cow"`, `Make Map` or `Inc 27`.
    Action { action: usize, value: Option<usize>, insert: Option<usize> },
}

impl RowTable {
    /// The columns of the logical view in reading order: `id` before `obj`,
    /// and `action`, `value` and `insert` merged into one `Action` column.
    pub fn shown(&self) -> Vec<(String, ShownCol)> {
        let pos = |n: &str| self.fields.iter().position(|f| f.name == n);
        let (action, value, insert) = (pos("action"), pos("value"), pos("insert"));
        let mut order: Vec<usize> = (0..self.fields.len()).collect();
        if let (Some(id), Some(obj)) = (pos("id"), pos("obj")) {
            if id > obj {
                order.remove(id);
                order.insert(obj, id);
            }
        }
        order
            .into_iter()
            .filter_map(|i| match action {
                Some(a) if i == a => Some(("Action".to_string(), ShownCol::Action { action: a, value, insert })),
                Some(_) if Some(i) == value || Some(i) == insert => None,
                _ => Some((self.fields[i].name.clone(), ShownCol::Field(i))),
            })
            .collect()
    }

    /// One cell of the logical view; see [`Field::render_with`] for `actors`.
    pub fn render_shown(&self, col: ShownCol, row: usize, actors: &[String]) -> String {
        match col {
            ShownCol::Field(i) => self.fields[i].render_with(row, actors),
            ShownCol::Action { action, value, insert } => {
                let FieldData::Scalar(v) = &self.fields[action].data else {
                    return self.fields[action].render_with(row, actors);
                };
                let Some(a) = v.get(row).and_then(Cell::as_u64) else {
                    return String::new();
                };
                let inserted = insert.is_some_and(|i| self.fields[i].render_with(row, actors) == "t");
                let value = value.map(|v| self.fields[v].render_with(row, actors)).filter(|v| !v.is_empty());
                action_label(ACTIONS.get(a as usize).copied().unwrap_or("?"), inserted, value.as_deref())
            }
        }
    }
}

impl Field {
    pub fn len(&self) -> usize {
        match &self.data {
            FieldData::Scalar(v) => v.len(),
            FieldData::Id { actor, ctr } => actor.len().max(ctr.len()),
            FieldData::Key { actor, ctr, string } => actor.len().max(ctr.len()).max(string.len()),
            FieldData::Value { meta, .. } => meta.len(),
            FieldData::Group { counts, .. } => counts.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn render(&self, row: usize) -> String {
        self.render_with(row, &[])
    }

    /// Like [`Field::render`], but shows ids as `counter@actor` with the
    /// first four hex digits of the actor from `actors`, and an empty object
    /// id as `ROOT`.
    pub fn render_with(&self, row: usize, actors: &[String]) -> String {
        let get = |v: &Vec<Cell>| v.get(row).cloned().unwrap_or(Cell::Null);
        let id = |a: Cell, c: Cell| {
            let actor = a
                .as_u64()
                .and_then(|i| actors.get(i as usize))
                .map(|hex| hex.chars().take(4).collect())
                .unwrap_or_else(|| a.to_string());
            format!("{c}@{actor}")
        };
        match &self.data {
            FieldData::Scalar(v) => {
                let c = get(v);
                if self.name == "action" {
                    if let Some(a) = c.as_u64() {
                        return ACTIONS
                            .get(a as usize)
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("action{a}"));
                    }
                }
                match c {
                    Cell::Null => String::new(),
                    Cell::B(b) => if b { "t" } else { "f" }.into(),
                    Cell::S(s) => truncate(&s, 60),
                    c => c.to_string(),
                }
            }
            FieldData::Id { actor, ctr } => match (get(actor), get(ctr)) {
                (Cell::Null, Cell::Null) if self.name == "obj" => "ROOT".into(),
                (Cell::Null, Cell::Null) => String::new(),
                (a, c) => id(a, c),
            },
            FieldData::Key { actor, ctr, string } => match get(string) {
                Cell::S(s) => truncate(&s, 40),
                _ => match (get(actor), get(ctr)) {
                    (_, Cell::I(0)) | (_, Cell::U(0)) => "_head".into(),
                    (Cell::Null, Cell::Null) => String::new(),
                    (a, c) => id(a, c),
                },
            },
            FieldData::Value { meta, offsets, raw } => {
                let Some(m) = meta.get(row).and_then(|m| m.as_u64()) else {
                    return String::new();
                };
                let (start, end) = (offsets[row], offsets[row + 1]);
                match raw.get(start..end) {
                    Some(bytes) if self.name == "extra" => slice_hex(bytes),
                    Some(bytes) => decode_value(m & 0xf, bytes).to_string(),
                    None => "<value out of range>".into(),
                }
            }
            FieldData::Group {
                starts, items, ..
            } => {
                let (Some(&s), Some(&e)) = (starts.get(row), starts.get(row + 1)) else {
                    return String::new();
                };
                if items.is_empty() || s == e {
                    return if s == e { String::new() } else { format!("×{}", e - s) };
                }
                let shown: Vec<String> = (s..e.min(s + 8))
                    .map(|k| items[0].render_with(k, actors))
                    .collect();
                let more = if e - s > 8 {
                    format!(" +{}", e - s - 8)
                } else {
                    String::new()
                };
                format!("{}{}", shown.join(", "), more)
            }
        }
    }
}

/// Rows and raw-value bytes per value type, from a value metadata column.
pub fn value_breakdown(meta: &ColumnInfo) -> Vec<(&'static str, usize, usize)> {
    let mut out: Vec<(&'static str, usize, usize)> = Vec::new();
    for cell in decode_cells(meta) {
        let Some(m) = cell.as_u64() else { continue };
        let name = value_type_name(m & 0xf);
        let len = (m >> 4) as usize;
        match out.iter_mut().find(|e| e.0 == name) {
            Some(e) => {
                e.1 += 1;
                e.2 += len;
            }
            None => out.push((name, 1, len)),
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.2));
    out
}
