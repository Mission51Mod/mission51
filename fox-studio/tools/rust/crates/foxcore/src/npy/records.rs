//! Borrowed fixed-width record access; payload bytes are never copied or cast.
use super::header::{self, Dtype};

/// Descriptor-derived byte range and optional C-order subarray dimensions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordField {
    pub name: String,
    pub descr: String,
    pub offset: usize,
    pub size: usize,
    pub shape: Vec<usize>,
}

impl RecordField {
    fn float_size(&self) -> Result<usize, String> {
        match self.descr.get(1..) {
            Some("f4") => Ok(4),
            Some("f8") => Ok(8),
            _ => Err(format!(
                "NPY: field {:?} requires float32/float64, got {:?}",
                self.name, self.descr
            )),
        }
    }

    fn require_scalar(&self) -> Result<(), String> {
        if self.shape.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "NPY: field {:?} is a subarray {:?}, expected a scalar",
                self.name, self.shape
            ))
        }
    }

    fn big_endian(&self) -> bool {
        self.descr.starts_with('>') || (self.descr.starts_with('=') && cfg!(target_endian = "big"))
    }
}

pub(super) struct RecordLayout {
    pub fields: Vec<RecordField>,
    pub size: usize,
}

/// Checked record metadata and borrowed payload, in the declared storage order.
/// No object/pickle, nested-record, complex or half-float fields are supported.
pub struct RecordArray<'a> {
    layout: RecordLayout,
    shape: Vec<usize>,
    fortran: bool,
    count: usize,
    data: &'a [u8],
}

impl<'a> RecordArray<'a> {
    pub fn read(bytes: &'a [u8]) -> Result<Self, String> {
        let (header, data) = header::read_file(bytes)?;
        let Dtype::Records(layout) = header.dtype else {
            return Err("NPY: expected a structured record descriptor".into());
        };
        let count = header::element_count(&header.shape)?;
        Ok(Self {
            layout,
            shape: header.shape,
            fortran: header.fortran,
            count,
            data,
        })
    }

    pub fn shape(&self) -> &[usize] {
        &self.shape
    }
    pub fn fortran(&self) -> bool {
        self.fortran
    }
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn record_size(&self) -> usize {
        self.layout.size
    }
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    pub fn field(&self, name: &str) -> Result<&RecordField, String> {
        find_field(&self.layout.fields, name)
    }

    /// Validate required coordinates even if this array contains no records.
    pub fn require_scalar_floats(&self, names: &[&str]) -> Result<(), String> {
        for name in names {
            let field = self.field(name)?;
            field.require_scalar()?;
            field.float_size()?;
        }
        Ok(())
    }

    /// Linear index in stored order; multidimensional callers inspect shape/order.
    pub fn record(&self, index: usize) -> Result<Record<'_>, String> {
        if index >= self.count {
            return Err(format!(
                "NPY: record index {index} is outside {} records",
                self.count
            ));
        }
        let start = index * self.layout.size; // payload size and count were checked at read
        Ok(Record {
            fields: &self.layout.fields,
            bytes: &self.data[start..start + self.layout.size],
        })
    }
}

/// A single borrowed record, with named access independent of field order.
pub struct Record<'a> {
    fields: &'a [RecordField],
    bytes: &'a [u8],
}

impl<'a> Record<'a> {
    pub fn field(&self, name: &str) -> Result<RecordValue<'a>, String> {
        let field = find_field(self.fields, name)?;
        Ok(RecordValue {
            field,
            bytes: &self.bytes[field.offset..field.offset + field.size],
        })
    }

    pub fn f64(&self, name: &str) -> Result<f64, String> {
        self.field(name)?.as_f64()
    }
    pub fn string(&self, name: &str) -> Result<String, String> {
        self.field(name)?.as_string()
    }
}

/// A checked field byte range. Floats are decoded from bytes without alignment assumptions.
pub struct RecordValue<'a> {
    field: &'a RecordField,
    bytes: &'a [u8],
}

impl<'a> RecordValue<'a> {
    pub fn info(&self) -> &'a RecordField {
        self.field
    }
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub fn as_f64(&self) -> Result<f64, String> {
        self.field.require_scalar()?;
        self.f64_at(0)
    }

    /// Index a scalar or the C-order elements of a fixed-shape subarray.
    pub fn f64_at(&self, index: usize) -> Result<f64, String> {
        let size = self.field.float_size()?;
        let start = index
            .checked_mul(size)
            .ok_or("NPY: field element index overflows")?;
        let end = start
            .checked_add(size)
            .ok_or("NPY: field element index overflows")?;
        let bytes = self.bytes.get(start..end).ok_or_else(|| {
            format!(
                "NPY: element {index} is outside field {:?}",
                self.field.name
            )
        })?;
        let big_endian = self.field.big_endian();
        Ok(if size == 4 {
            let bits = bytes.try_into().unwrap();
            if big_endian {
                f32::from_be_bytes(bits) as f64
            } else {
                f32::from_le_bytes(bits) as f64
            }
        } else {
            let bits = bytes.try_into().unwrap();
            if big_endian {
                f64::from_be_bytes(bits)
            } else {
                f64::from_le_bytes(bits)
            }
        })
    }

    /// Decode a scalar byte/Unicode string, trimming trailing NUL padding only.
    pub fn as_string(&self) -> Result<String, String> {
        self.field.require_scalar()?;
        match self.field.descr.as_bytes()[1] {
            b'S' => {
                let end = self
                    .bytes
                    .iter()
                    .rposition(|&byte| byte != 0)
                    .map_or(0, |last| last + 1);
                std::str::from_utf8(&self.bytes[..end])
                    .map(str::to_owned)
                    .map_err(|error| {
                        format!("NPY: field {:?} is not UTF-8: {error}", self.field.name)
                    })
            }
            b'U' => {
                let end = self
                    .bytes
                    .chunks_exact(4)
                    .rposition(|bytes| bytes != [0; 4])
                    .map_or(0, |last| last + 1);
                let mut text = String::new();
                for bytes in self.bytes[..end * 4].chunks_exact(4) {
                    let bits = bytes.try_into().unwrap();
                    let codepoint = if self.field.big_endian() {
                        u32::from_be_bytes(bits)
                    } else {
                        u32::from_le_bytes(bits)
                    };
                    text.push(char::from_u32(codepoint).ok_or_else(|| {
                        format!(
                            "NPY: field {:?} has invalid Unicode code point 0x{codepoint:x}",
                            self.field.name
                        )
                    })?);
                }
                Ok(text)
            }
            _ => Err(format!(
                "NPY: field {:?} requires a byte/Unicode string, got {:?}",
                self.field.name, self.field.descr
            )),
        }
    }
}

fn find_field<'a>(fields: &'a [RecordField], name: &str) -> Result<&'a RecordField, String> {
    fields
        .iter()
        .find(|field| field.name == name)
        .ok_or_else(|| format!("NPY: missing record field {name:?}"))
}
