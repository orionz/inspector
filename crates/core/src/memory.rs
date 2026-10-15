//! A stored column loaded into hexane the way automerge holds it in memory.

use std::sync::Arc;

pub use hexane::debug::{NodeDebug, SlabDebug};
use hexane::{Codec, DeltaColumn, PackError, PrefixColumn, Run as HexRun};

use crate::columns::{decode_runs, Cell, ColType, ColumnInfo, Run, RunKind};

/// How the column's values relate to what its slabs store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryKind {
    /// Slabs hold the values themselves.
    Plain,
    /// Slabs hold deltas; values are running sums across the whole column.
    Delta,
    /// The index also keeps a running sum: of true values, of counts, or of
    /// value lengths.
    Prefix,
}

pub struct MemorySlab {
    pub debug: SlabDebug,
    /// The slab's bytes decoded into runs, rows counted from the slab's start.
    pub runs: Vec<Run>,
    /// For delta and prefix columns, what the runs mean once the rest of
    /// the column is taken into account.
    pub realized: Vec<String>,
}

pub struct ColumnMemory {
    /// The hexane type automerge uses for this column.
    pub type_name: &'static str,
    pub kind: MemoryKind,
    pub index: Option<NodeDebug>,
    pub slabs: Vec<MemorySlab>,
}

/// Loads the column into the hexane type automerge uses for it. Value
/// columns have no slabs (automerge keeps them as one raw buffer), so they
/// return an error saying so.
pub fn column_memory(col: &ColumnInfo) -> Result<ColumnMemory, String> {
    let data = &col.data[..];
    let err = |e: PackError| format!("hexane could not load this column: {e}");
    let (type_name, kind, index, slabs) = match col.col_type() {
        ColType::Value => return Err("automerge keeps value bytes in a single RawColumn buffer, without slabs or an index.".into()),
        ColType::DeltaInteger => {
            let c = DeltaColumn::<Option<i64>>::load(data).map_err(err)?;
            ("DeltaColumn<Option<i64>>", MemoryKind::Delta, Some(c.debug_index()), c.debug_slabs())
        }
        ColType::Group => {
            let c = PrefixColumn::<u64>::load(data).map_err(err)?;
            ("PrefixColumn<u64>", MemoryKind::Prefix, Some(c.debug_index()), c.debug_slabs())
        }
        ColType::ValueMetadata => {
            let c = PrefixColumn::<ValueMeta>::load(data).map_err(err)?;
            ("PrefixColumn<ValueMeta>", MemoryKind::Prefix, Some(c.debug_index()), c.debug_slabs())
        }
        ColType::Boolean if col.name == "insert" => {
            let c = PrefixColumn::<bool>::load(data).map_err(err)?;
            ("PrefixColumn<bool>", MemoryKind::Prefix, Some(c.debug_index()), c.debug_slabs())
        }
        ColType::Boolean => {
            let c = hexane::Column::<bool>::load(data).map_err(err)?;
            ("Column<bool>", MemoryKind::Plain, Some(c.debug_index()), c.debug_slabs())
        }
        ColType::String => {
            let c = hexane::Column::<Option<String>>::load(data).map_err(err)?;
            ("Column<Option<String>>", MemoryKind::Plain, Some(c.debug_index()), c.debug_slabs())
        }
        ColType::Actor | ColType::Integer => {
            let c = hexane::Column::<Option<u64>>::load(data).map_err(err)?;
            ("Column<Option<u64>>", MemoryKind::Plain, Some(c.debug_index()), c.debug_slabs())
        }
    };
    let mut running: i64 = 0;
    let mut prefix: u64 = 0;
    let slabs = slabs
        .into_iter()
        .map(|debug| {
            let slab_col = ColumnInfo { data: Arc::from(debug.bytes.as_slice()), file_offset: None, stored_len: debug.bytes.len(), ..col.clone() };
            let runs = decode_runs(&slab_col).runs;
            let realized = match kind {
                MemoryKind::Plain => vec![],
                MemoryKind::Delta => runs.iter().map(|r| realize_delta(r, &mut running)).collect(),
                MemoryKind::Prefix => {
                    let total: u64 = runs.iter().map(|r| prefix_weight(col.col_type(), r)).sum();
                    let line = format!("Prefix={prefix}, Total={total}, Count={}", debug.len);
                    prefix += total;
                    vec![line]
                }
            };
            MemorySlab { debug, runs, realized }
        })
        .collect();
    Ok(ColumnMemory { type_name, kind, index, slabs })
}

fn realize_delta(r: &Run, running: &mut i64) -> String {
    match r.kind {
        RunKind::Null => format!("NULL x{}", r.count),
        RunKind::Repeat => {
            let step = cell_i64(r.values.first());
            let start = *running + step;
            *running += step * r.count as i64;
            match (r.count, step) {
                (1, _) => start.to_string(),
                (_, 0) => format!("{start} x{}", r.count),
                (_, 1) => format!("{start}..{}", *running),
                _ => format!("{start}..{} Δ {step}", *running),
            }
        }
        _ => r
            .values
            .iter()
            .map(|v| match v {
                Cell::Null => "NULL".to_string(),
                v => {
                    *running += cell_i64(Some(v));
                    running.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(", "),
    }
}

fn prefix_weight(ty: ColType, r: &Run) -> u64 {
    let each = |v: &Cell| match (ty, v) {
        (_, Cell::B(b)) => *b as u64,
        (ColType::ValueMetadata, Cell::U(v)) => v >> 4,
        (_, Cell::U(v)) => *v,
        _ => 0,
    };
    match r.kind {
        RunKind::Repeat | RunKind::Bool => r.values.first().map_or(0, each) * r.count as u64,
        _ => r.values.iter().map(each).sum(),
    }
}

fn cell_i64(c: Option<&Cell>) -> i64 {
    match c {
        Some(Cell::I(v)) => *v,
        Some(Cell::U(v)) => *v as i64,
        _ => 0,
    }
}

/// A value metadata entry: the value's byte length shifted left four bits
/// over its type code. Its prefix sum is the total value length, as in automerge.
#[derive(Copy, Clone, Debug, Default, PartialEq, PartialOrd)]
pub struct ValueMeta(u64);

impl hexane::ColumnValue for ValueMeta {
    type Encoding<C: Codec> = hexane::RleEncoding<ValueMeta, C>;
}

impl hexane::RleValue for ValueMeta {
    fn try_unpack<C: Codec>(data: &[u8]) -> Result<(usize, ValueMeta), PackError> {
        let (n, v) = C::try_read_unsigned(data)?;
        Ok((n, ValueMeta(v)))
    }
    fn pack<C: Codec>(value: ValueMeta, out: &mut Vec<u8>) -> bool {
        out.extend(C::encode_unsigned(value.0));
        true
    }
}

impl hexane::PrefixValue for ValueMeta {
    type Prefix = u64;
    fn accumulate(target: &mut u64, val: ValueMeta) {
        *target += val.0 >> 4;
    }
    fn accumulate_run(target: &mut u64, run: &HexRun<ValueMeta>) {
        *target += (run.value.0 >> 4) * run.count as u64;
    }
}
