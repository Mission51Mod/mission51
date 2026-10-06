//! Checked NPY envelope and the supported Python-literal header grammar.
use super::records::{RecordField, RecordLayout};
use std::{borrow::Cow, collections::BTreeSet};

pub(super) enum Dtype {
    Simple(String),
    Records(RecordLayout),
}

pub(super) struct Header {
    pub descr_raw: String,
    pub dtype: Dtype,
    pub fortran: bool,
    pub shape: Vec<usize>,
}

/// Both owned numeric arrays and borrowed records use this one parser.
pub(super) fn read_file(bytes: &[u8]) -> Result<(Header, &[u8]), String> {
    if bytes.get(..6) != Some(b"\x93NUMPY") {
        return Err("NPY: missing magic bytes".into());
    }
    let version = bytes.get(6..8).ok_or("NPY: truncated version")?;
    let (header_length, header_start) = match version {
        [1, 0] => {
            let length = bytes
                .get(8..10)
                .ok_or("NPY: truncated version-1 header length")?;
            (
                u16::from_le_bytes(length.try_into().unwrap()) as usize,
                10usize,
            )
        }
        [2 | 3, 0] => {
            let length = bytes
                .get(8..12)
                .ok_or("NPY: truncated version-2/3 header length")?;
            (
                u32::from_le_bytes(length.try_into().unwrap()) as usize,
                12usize,
            )
        }
        [major, minor] => return Err(format!("NPY: unsupported version {major}.{minor}")),
        _ => unreachable!(),
    };
    let header_end = header_start
        .checked_add(header_length)
        .ok_or("NPY: header length overflow")?;
    let header_bytes = bytes
        .get(header_start..header_end)
        .ok_or("NPY: truncated header")?;
    if !header_bytes.ends_with(b"\n") {
        return Err("NPY: header must end with a newline".into());
    }
    // Versions 1/2 specify Latin-1; version 3 specifies UTF-8 field names.
    let text = if version[0] == 3 {
        Cow::Borrowed(
            std::str::from_utf8(header_bytes)
                .map_err(|error| format!("NPY: invalid UTF-8 header: {error}"))?,
        )
    } else if header_bytes.is_ascii() {
        Cow::Borrowed(std::str::from_utf8(header_bytes).unwrap())
    } else {
        Cow::Owned(
            header_bytes
                .iter()
                .map(|&byte| char::from(byte))
                .collect::<String>(),
        )
    };
    let header = HeaderReader::new(&text).read()?;
    let element_size = match &header.dtype {
        Dtype::Simple(descr) => dtype_size(descr)?,
        Dtype::Records(layout) => layout.size,
    };
    let payload = &bytes[header_end..];
    validate_size(element_size, &header.shape, payload.len())?;
    Ok((header, payload))
}

pub(super) fn element_count(shape: &[usize]) -> Result<usize, String> {
    if shape.contains(&0) {
        return Ok(0);
    }
    shape
        .iter()
        .try_fold(1usize, |product, &size| product.checked_mul(size))
        .ok_or_else(|| "NPY: shape product overflows".into())
}

pub(super) fn validate_size(
    element_size: usize,
    shape: &[usize],
    length: usize,
) -> Result<(), String> {
    let expected = element_count(shape)?
        .checked_mul(element_size)
        .ok_or("NPY: payload size overflows")?;
    if length != expected {
        return Err(format!(
            "NPY: shape {shape:?} with {element_size}-byte elements needs {expected} payload bytes, got {length}"
        ));
    }
    Ok(())
}

pub(super) fn dtype_size(descr: &str) -> Result<usize, String> {
    let bytes = descr.as_bytes();
    if bytes.len() < 3 || !matches!(bytes[0], b'<' | b'>' | b'=' | b'|') {
        return Err(format!("NPY: unsupported dtype {descr:?}"));
    }
    let width = descr
        .get(2..)
        .and_then(|suffix| suffix.parse::<usize>().ok())
        .ok_or_else(|| format!("NPY: unsupported dtype {descr:?}"))?;
    let supported = match bytes[1] {
        b'b' => width == 1,
        b'i' | b'u' => matches!(width, 1 | 2 | 4 | 8),
        b'f' => matches!(width, 2 | 4 | 8),
        b'c' => matches!(width, 8 | 16),
        b'S' | b'V' | b'U' => true,
        _ => false,
    };
    if !supported || (bytes[0] == b'|' && width > 1 && !matches!(bytes[1], b'S' | b'V')) {
        return Err(format!("NPY: unsupported dtype {descr:?}"));
    }
    if bytes[1] == b'U' {
        width
            .checked_mul(4)
            .ok_or_else(|| "NPY: Unicode dtype size overflows".into())
    } else {
        Ok(width)
    }
}

/// No evaluator, pickle, nested records or executable expressions are accepted.
pub(super) struct HeaderReader<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> HeaderReader<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { text, at: 0 }
    }

    pub fn read(mut self) -> Result<Header, String> {
        let (mut dtype, mut fortran, mut shape) = (None, None, None);
        self.expect(b'{')?;
        while !self.consume(b'}') {
            let (_, key) = self.quoted()?;
            self.expect(b':')?;
            match key.as_str() {
                "descr" if dtype.is_none() => dtype = Some(self.descriptor()?),
                "fortran_order" if fortran.is_none() => fortran = Some(self.boolean()?),
                "shape" if shape.is_none() => shape = Some(self.shape()?),
                "descr" | "fortran_order" | "shape" => {
                    return Err(format!("NPY: duplicate header field {key:?}"));
                }
                _ => return Err(format!("NPY: unsupported header field {key:?}")),
            }
            if self.consume(b'}') {
                break;
            }
            self.expect(b',')?;
        }
        self.spaces();
        if self.at != self.text.len() {
            return Err("NPY: unexpected text after header dictionary".into());
        }
        let (descr_raw, dtype) = dtype.ok_or("NPY: header is missing descr")?;
        Ok(Header {
            descr_raw,
            dtype,
            fortran: fortran.ok_or("NPY: header is missing fortran_order")?,
            shape: shape.ok_or("NPY: header is missing shape")?,
        })
    }

    fn descriptor(&mut self) -> Result<(String, Dtype), String> {
        self.spaces();
        let start = self.at;
        let dtype = if self.consume(b'[') {
            Dtype::Records(self.record_layout()?)
        } else {
            let (_, descr) = self.quoted()?;
            dtype_size(&descr)?;
            Dtype::Simple(descr)
        };
        Ok((self.text[start..self.at].to_string(), dtype))
    }

    fn record_layout(&mut self) -> Result<RecordLayout, String> {
        let mut fields = Vec::new();
        let mut names = BTreeSet::new();
        let mut offset = 0usize;
        while !self.consume(b']') {
            self.expect(b'(')?;
            let (_, name) = self.quoted()?;
            self.expect(b',')?;
            let (_, descr) = self.quoted()?;
            let element_size = dtype_size(&descr)?;
            if matches!(descr.as_bytes()[1], b'c')
                || (descr.as_bytes()[1] == b'f' && element_size == 2)
            {
                return Err(format!("NPY: unsupported record field dtype {descr:?}"));
            }
            let mut shape = Vec::new();
            if self.consume(b',') {
                if !self.consume(b')') {
                    shape = self.shape()?;
                    self.consume(b',');
                    self.expect(b')')?;
                }
            } else {
                self.expect(b')')?;
            }
            let size = element_count(&shape)?
                .checked_mul(element_size)
                .ok_or("NPY: record field size overflows")?;
            if name.is_empty() {
                // NPY represents explicit gaps/alignment as anonymous void fields.
                if descr.as_bytes()[1] != b'V' || !shape.is_empty() {
                    return Err("NPY: anonymous record fields must be scalar void padding".into());
                }
            } else {
                if !names.insert(name.clone()) {
                    return Err(format!("NPY: duplicate record field {name:?}"));
                }
                fields.push(RecordField {
                    name,
                    descr,
                    offset,
                    size,
                    shape,
                });
            }
            offset = offset
                .checked_add(size)
                .ok_or("NPY: record size overflows")?;
            if self.consume(b']') {
                break;
            }
            self.expect(b',')?;
        }
        if offset == 0 {
            return Err("NPY: zero-sized record descriptors are unsupported".into());
        }
        Ok(RecordLayout {
            fields,
            size: offset,
        })
    }

    fn spaces(&mut self) {
        while self
            .text
            .as_bytes()
            .get(self.at)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.at += 1;
        }
    }

    fn consume(&mut self, expected: u8) -> bool {
        self.spaces();
        if self.text.as_bytes().get(self.at) == Some(&expected) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, expected: u8) -> Result<(), String> {
        if self.consume(expected) {
            Ok(())
        } else {
            Err(format!(
                "NPY: expected {:?} at header byte {}",
                expected as char, self.at
            ))
        }
    }

    fn quoted(&mut self) -> Result<(String, String), String> {
        self.spaces();
        let start = self.at;
        let quote = *self
            .text
            .as_bytes()
            .get(start)
            .ok_or("NPY: truncated quoted string")?;
        if !matches!(quote, b'\'' | b'"') {
            return Err("NPY: expected a quoted string".into());
        }
        self.at += 1;
        let value_start = self.at;
        while let Some(&byte) = self.text.as_bytes().get(self.at) {
            if byte == quote {
                let value = self.text[value_start..self.at].to_string();
                self.at += 1;
                return Ok((self.text[start..self.at].to_string(), value));
            }
            if byte == b'\\' || byte < b' ' {
                return Err(
                    "NPY: escapes/control characters in header strings are unsupported".into(),
                );
            }
            self.at += 1;
        }
        Err("NPY: unterminated header string".into())
    }

    fn boolean(&mut self) -> Result<bool, String> {
        self.spaces();
        for (word, value) in [("True", true), ("False", false)] {
            if self.text[self.at..].starts_with(word) {
                self.at += word.len();
                return Ok(value);
            }
        }
        Err("NPY: fortran_order must be True or False".into())
    }

    fn shape(&mut self) -> Result<Vec<usize>, String> {
        self.expect(b'(')?;
        let mut dimensions = Vec::new();
        if self.consume(b')') {
            return Ok(dimensions);
        }
        loop {
            self.spaces();
            let start = self.at;
            while self
                .text
                .as_bytes()
                .get(self.at)
                .is_some_and(u8::is_ascii_digit)
            {
                self.at += 1;
            }
            if self.at == start {
                return Err("NPY: shape dimensions must be nonnegative integers".into());
            }
            let size = self.text[start..self.at]
                .parse()
                .map_err(|_| "NPY: shape dimension overflows")?;
            dimensions.push(size);
            if self.consume(b',') {
                if self.consume(b')') {
                    return Ok(dimensions);
                }
            } else {
                self.expect(b')')?;
                if dimensions.len() == 1 {
                    return Err("NPY: one-dimensional shape needs a trailing comma".into());
                }
                return Ok(dimensions);
            }
        }
    }
}
