use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub offset: usize,
    pub what: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.what, self.offset)
    }
}

impl std::error::Error for ParseError {}

pub type Result<T> = std::result::Result<T, ParseError>;

/// A cursor over a byte slice whose positions are reported relative to `base`,
/// so offsets in errors and sections line up with the original file.
#[derive(Clone)]
pub struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    base: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8], base: usize) -> Self {
        Self { bytes, pos: 0, base }
    }

    pub fn pos(&self) -> usize {
        self.base + self.pos
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub fn rest(&self) -> &'a [u8] {
        &self.bytes[self.pos..]
    }

    pub fn err<T>(&self, what: impl Into<String>) -> Result<T> {
        Err(ParseError {
            offset: self.pos(),
            what: what.into(),
        })
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return self.err(format!(
                "wanted {n} bytes but only {} remain",
                self.remaining()
            ));
        }
        let out = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    pub fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn uleb(&mut self) -> Result<u64> {
        match read_uleb(self.rest()) {
            Some((v, n)) => {
                self.pos += n;
                Ok(v)
            }
            None => self.err("bad unsigned LEB128"),
        }
    }

    pub fn sleb(&mut self) -> Result<i64> {
        match read_sleb(self.rest()) {
            Some((v, n)) => {
                self.pos += n;
                Ok(v)
            }
            None => self.err("bad signed LEB128"),
        }
    }

    /// A uleb count, refused if the rest of the input couldn't hold that many
    /// entries of at least `min_entry` bytes.
    pub fn count(&mut self, min_entry: usize) -> Result<usize> {
        let n = self.uleb()?;
        if n as u128 * min_entry as u128 > self.remaining() as u128 {
            return self.err(format!("count {n} exceeds remaining input"));
        }
        Ok(n as usize)
    }

    pub fn len_prefixed(&mut self) -> Result<&'a [u8]> {
        let n = self.uleb()?;
        if n > self.remaining() as u64 {
            return self.err(format!("length {n} exceeds remaining input"));
        }
        self.take(n as usize)
    }

    pub fn hash(&mut self) -> Result<[u8; 32]> {
        Ok(self.take(32)?.try_into().unwrap())
    }
}

pub fn read_uleb(b: &[u8]) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift = 0;
    for (i, &byte) in b.iter().enumerate() {
        if shift >= 64 {
            return None;
        }
        result |= ((byte & 0x7f) as u64) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            return Some((result, i + 1));
        }
    }
    None
}

pub fn read_sleb(b: &[u8]) -> Option<(i64, usize)> {
    let mut result: i64 = 0;
    let mut shift = 0;
    for (i, &byte) in b.iter().enumerate() {
        if shift >= 64 {
            return None;
        }
        result |= ((byte & 0x7f) as i64) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            if shift < 64 && byte & 0x40 != 0 {
                result |= -1i64 << shift;
            }
            return Some((result, i + 1));
        }
    }
    None
}

pub fn write_uleb(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb_roundtrip() {
        for v in [0u64, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
            let mut out = Vec::new();
            write_uleb(&mut out, v);
            assert_eq!(read_uleb(&out), Some((v, out.len())));
        }
        assert_eq!(read_sleb(&[0x7f]), Some((-1, 1)));
        assert_eq!(read_sleb(&[0x80, 0x7f]), Some((-128, 2)));
        assert_eq!(read_sleb(&[0x3f]), Some((63, 1)));
    }
}
