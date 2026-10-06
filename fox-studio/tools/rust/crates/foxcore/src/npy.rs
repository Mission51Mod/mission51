//! NPY arrays, read and written without a NumPy or Python runtime.
//!
//! Additive relocation of our format implementation in `foxgeo::npy`, with a
//! checked header reader and explicit dtype/layout conversion for previews.
//! The format specification is <https://numpy.org/doc/stable/reference/generated/numpy.lib.format.html>.
//! Versions 1.0, 2.0 and 3.0 are read. `Npy` holds simple fixed-width arrays;
//! `RecordArray` borrows checked structured records through the same parser.
//! Object/pickle payloads and datetime units return an unsupported-format error. The canonical writer retains our existing bytes
//! for simple arrays (alphabetical fields, 64-byte alignment, version 1 or 2).

mod header;
mod records;

use header::{Dtype, HeaderReader, dtype_size, validate_size};
pub use records::{Record, RecordArray, RecordField, RecordValue};

#[derive(Clone, Debug, PartialEq)]
pub struct Npy {
    /// Quoted simple dtype exactly as it appeared in the header.
    pub descr_raw: String,
    /// Simple dtype without quotes, such as `<f4` or `|u1`.
    pub descr: String,
    pub fortran: bool,
    pub shape: Vec<usize>,
    /// Element bytes in the declared byte order and C/Fortran storage order.
    pub data: Vec<u8>,
}

impl Npy {
    pub fn read(bytes: &[u8]) -> Result<Self, String> {
        let (header, payload) = header::read_file(bytes)?;
        let Dtype::Simple(descr) = header.dtype else {
            return Err("NPY: structured descriptors require RecordArray".into());
        };
        Ok(Self {
            descr_raw: header.descr_raw,
            descr,
            fortran: header.fortran,
            shape: header.shape,
            data: payload.to_vec(),
        })
    }

    /// Canonical serializer, matching our existing simple-array writer.
    /// Use `try_write` to validate a manually constructed array first.
    pub fn write(&self) -> Vec<u8> {
        let shape = match self.shape.as_slice() {
            [size] => format!("({size},)"),
            sizes => format!(
                "({})",
                sizes
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        let body = format!(
            "{{'descr': {}, 'fortran_order': {}, 'shape': {shape}, }}",
            self.descr_raw,
            if self.fortran { "True" } else { "False" }
        );
        let mut header = padded_header(&body, 10);
        let mut out = if header.len() <= u16::MAX as usize {
            let mut prefix = b"\x93NUMPY\x01\x00".to_vec();
            prefix.extend_from_slice(&(header.len() as u16).to_le_bytes());
            prefix
        } else {
            header = padded_header(&body, 12);
            let mut prefix = b"\x93NUMPY\x02\x00".to_vec();
            prefix.extend_from_slice(&(header.len() as u32).to_le_bytes());
            prefix
        };
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(&self.data);
        out
    }

    pub fn try_write(&self) -> Result<Vec<u8>, String> {
        self.validate_payload()?;
        let quoted = format!(
            "{{'descr': {}, 'fortran_order': False, 'shape': (), }}",
            self.descr_raw
        );
        let header = HeaderReader::new(&quoted).read()?;
        if !matches!(header.dtype, Dtype::Simple(ref descr) if descr == &self.descr) {
            return Err("NPY: descr and descr_raw disagree".into());
        }
        Ok(self.write())
    }

    pub fn elem_size(&self) -> usize {
        dtype_size(&self.descr).unwrap_or(1)
    }

    /// Stored-order floats. For checked dtype and layout, use `to_f32_c_order`.
    pub fn f32s(&self) -> Vec<f32> {
        self.data
            .chunks_exact(4)
            .map(|bytes| {
                let bits = bytes.try_into().unwrap();
                if self.big_endian() {
                    f32::from_be_bytes(bits)
                } else {
                    f32::from_le_bytes(bits)
                }
            })
            .collect()
    }

    /// Stored-order doubles, using the declared byte order.
    pub fn f64s(&self) -> Vec<f64> {
        self.data
            .chunks_exact(8)
            .map(|bytes| {
                let bits = bytes.try_into().unwrap();
                if self.big_endian() {
                    f64::from_be_bytes(bits)
                } else {
                    f64::from_le_bytes(bits)
                }
            })
            .collect()
    }

    /// Float32/float64 values in row-major order, suitable for a preview grid.
    pub fn to_f32_c_order(&self) -> Result<Vec<f32>, String> {
        self.validate_payload()?;
        let values = match self.descr.get(1..) {
            Some("f4") => self.f32s(),
            Some("f8") => self.f64s().into_iter().map(|value| value as f32).collect(),
            _ => {
                return Err(format!(
                    "NPY: expected float32 or float64, got {:?}",
                    self.descr
                ));
            }
        };
        if !self.fortran || self.shape.len() < 2 || values.is_empty() {
            return Ok(values);
        }
        let mut stride = 1usize;
        let mut fortran_strides = Vec::with_capacity(self.shape.len());
        for &dimension in &self.shape {
            fortran_strides.push(stride);
            stride *= dimension; // checked shape product in validate_payload
        }
        let mut row_major = Vec::with_capacity(values.len());
        for index in 0..values.len() {
            let mut remainder = index;
            let mut stored_index = 0;
            for axis in (0..self.shape.len()).rev() {
                stored_index += (remainder % self.shape[axis]) * fortran_strides[axis];
                remainder /= self.shape[axis];
            }
            row_major.push(values[stored_index]);
        }
        Ok(row_major)
    }

    pub fn from_f32(shape: Vec<usize>, values: &[f32]) -> Self {
        Self {
            descr_raw: "'<f4'".into(),
            descr: "<f4".into(),
            fortran: false,
            shape,
            data: values
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect(),
        }
    }

    pub fn from_f64(shape: Vec<usize>, values: &[f64]) -> Self {
        Self {
            descr_raw: "'<f8'".into(),
            descr: "<f8".into(),
            fortran: false,
            shape,
            data: values
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect(),
        }
    }

    pub fn from_u8(shape: Vec<usize>, values: &[u8]) -> Self {
        Self {
            descr_raw: "'|u1'".into(),
            descr: "|u1".into(),
            fortran: false,
            shape,
            data: values.to_vec(),
        }
    }

    fn big_endian(&self) -> bool {
        self.descr.starts_with('>') || (self.descr.starts_with('=') && cfg!(target_endian = "big"))
    }

    fn validate_payload(&self) -> Result<(), String> {
        validate_size(dtype_size(&self.descr)?, &self.shape, self.data.len())
    }
}

fn padded_header(body: &str, prefix_size: usize) -> String {
    let padding = (64 - (prefix_size + body.len() + 1) % 64) % 64;
    format!("{body}{}\n", " ".repeat(padding))
}
