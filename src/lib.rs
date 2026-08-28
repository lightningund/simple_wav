use std::io::{Read, Seek, Write};
use std::io::Result as ioResult;

use thiserror::Error;
use bytemuck::cast_slice;

/// Trait representing a type that can be used to read and seek.
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SampleType {
	#[default]
	I16,
	I32,
	F32,
	F64
}

#[derive(Error, Debug)]
pub enum SampleTypeError {
	#[error("Invalid PCM sample width {0}")]
	InvalidPCMWidth(u16),
	#[error("Invalid IEEE Float sample width {0}")]
	InvalidFloatWidth(u16),
	#[error("Unsupported Format {0}")]
	UnsupportedFormat(u16),
}

#[derive(Error, Debug)]
pub enum WavError {
	#[error("{0}")]
	IOError(#[from] std::io::Error),
	#[error("{0}")]
	SampleTypeError(#[from] SampleTypeError),
	#[error("File is missing the fmt chunk")]
	MissingFMT,
	#[error("File is missing the data chunk")]
	MissingData,
	#[error("File is missing the beginning RIFF tag")]
	RIFFInvalid,
	#[error("File is missing the beginning WAVE tag")]
	WAVEInvalid,
}

type WavResult<T> = Result<T, WavError>;

/// Module containing the functionality for converting between the supported audio sample types
mod conversion {
	/// Trait for converting between audio sample types
	pub trait ConvertTo<T> {
		fn convert_to(&self) -> T;
	}

	// Trivial implementation for converting to self
	impl<T: Copy> ConvertTo<T> for T {
		fn convert_to(&self) -> T {
			*self
		}
	}

	// Integer -> Integer
	impl ConvertTo<i32> for i16 {
		fn convert_to(&self) -> i32 {
			(*self as i32) << 16
		}
	}
	impl ConvertTo<i16> for i32 {
		fn convert_to(&self) -> i16 {
			(*self >> 16) as i16
		}
	}

	// Integer -> Float
	// Just divide by the maximum to get a range of -1 to 1
	impl ConvertTo<f32> for i16 {
		fn convert_to(&self) -> f32 {
			(*self as f32) / (i16::MAX as f32)
		}
	}
	impl ConvertTo<f64> for i16 {
		fn convert_to(&self) -> f64 {
			(*self as f64) / (i16::MAX as f64)
		}
	}
	impl ConvertTo<f32> for i32 {
		fn convert_to(&self) -> f32 {
			(*self as f32) / (i32::MAX as f32)
		}
	}
	impl ConvertTo<f64> for i32 {
		fn convert_to(&self) -> f64 {
			(*self as f64) / (i32::MAX as f64)
		}
	}

	// Float -> Integer
	// As of Rust 1.45 casting to int from float with "as" performs a saturating cast
	impl ConvertTo<i16> for f32 {
		fn convert_to(&self) -> i16 {
			(*self * (i16::MAX as f32)) as i16
		}
	}
	impl ConvertTo<i32> for f32 {
		fn convert_to(&self) -> i32 {
			(*self * (i32::MAX as f32)) as i32
		}
	}
	impl ConvertTo<i16> for f64 {
		fn convert_to(&self) -> i16 {
			(*self * (i16::MAX as f64)) as i16
		}
	}
	impl ConvertTo<i32> for f64 {
		fn convert_to(&self) -> i32 {
			(*self * (i32::MAX as f64)) as i32
		}
	}

	// Float -> Float
	impl ConvertTo<f64> for f32 {
		fn convert_to(&self) -> f64 {
			*self as f64
		}
	}
	impl ConvertTo<f32> for f64 {
		fn convert_to(&self) -> f32 {
			*self as f32
		}
	}

	// From raw bytes
	impl ConvertTo<i16> for [u8; 2] {
		fn convert_to(&self) -> i16 {
			i16::from_le_bytes(*self)
		}
	}
	impl ConvertTo<i32> for [u8; 4] {
		fn convert_to(&self) -> i32 {
			i32::from_le_bytes(*self)
		}
	}
	impl ConvertTo<f32> for [u8; 4] {
		fn convert_to(&self) -> f32 {
			f32::from_le_bytes(*self)
		}
	}
	impl ConvertTo<f64> for [u8; 8] {
		fn convert_to(&self) -> f64 {
			f64::from_le_bytes(*self)
		}
	}
}

const RIFF_ID: [u8; 4] = *b"RIFF";
const WAVE_ID: [u8; 4] = *b"WAVE";
const FMT_ID: [u8; 4] = *b"fmt ";
const FACT_ID: [u8; 4] = *b"fact";
const DATA_ID: [u8; 4] = *b"data";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
#[allow(non_camel_case_types)]
enum WaveFormat {
	PCM = 0x0001,
	IEEE_FLOAT = 0x0003,
	ALAW = 0x0006,
	MULAW = 0x0007,
	EXTENSIBLE = 0xFFFE,
}

/// Make sure the next however many bytes are the same as the ones passed
///
/// Advances the reader forward by N
fn confirm_bytes<R: ReadSeek, const N: usize>(reader: &mut R, target: [u8; N]) -> bool {
	let mut buf = [0; N];
	// we unwrap here because if anything goes wrong we're gonna panic at the assert anyway
	reader.read_exact(&mut buf).unwrap();
	buf == target
}

/// Reads two bytes and converts them to a u16, assuming little endian
fn read_u16<R: ReadSeek>(reader: &mut R) -> ioResult<u16> {
	let mut buf = [0; 2];
	reader.read_exact(&mut buf)?;
	Ok(u16::from_le_bytes(buf))
}

/// Reads four bytes and converts them to a u32, assuming little endian
fn read_u32<R: ReadSeek>(reader: &mut R) -> ioResult<u32> {
	let mut buf = [0; 4];
	reader.read_exact(&mut buf)?;
	Ok(u32::from_le_bytes(buf))
}

/// Important information we get out of the format chunk
#[derive(Default, Debug, Clone, Copy)]
struct WavFormat {
	channels: u16, // The number of channels
	sample_rate: u32, // The sample rate
	sample_type: SampleType, // The sample type
}

/// Just the absolute minimum of important information in a wav file
#[derive(Debug, Clone)]
struct MinWav {
	format: WavFormat, // Information about the structure of the audio
	data_buffer: Vec<u8>, // Raw buffer of the samples
}

/// Get the entry in the enum based on the format and number of bits per sample
fn determine_sample_type(wave_format: u16, sample_size: u16) -> Result<SampleType, SampleTypeError> {
	if wave_format == WaveFormat::PCM as u16 {
		if sample_size == 16 { Ok(SampleType::I16) }
		else if sample_size == 32 { Ok(SampleType::I32) }
		else { Err(SampleTypeError::InvalidPCMWidth(sample_size)) }
	} else if wave_format == WaveFormat::IEEE_FLOAT as u16 {
		if sample_size == 32 { Ok(SampleType::F32) }
		else if sample_size == 64 { Ok(SampleType::F64) }
		else { Err(SampleTypeError::InvalidFloatWidth(sample_size)) }
	} else {
		Err(SampleTypeError::UnsupportedFormat(wave_format))
	}
}

/// Read in the fmt chunk and return the important data
///
/// Supports 16, 18, and 40 byte headers
///
/// Note: Expects the `fmt ` bytes to have already been read
fn read_fmt_chunk<R: ReadSeek>(reader: &mut R) -> WavResult<WavFormat> {
	// First 4: Size of this chunk
	let chunk_size = read_u32(reader)?;
	// Next 2: WaveFormat
	let mut wave_format = read_u16(reader)?;
	// Next 2: # Channels
	let channels = read_u16(reader)?;
	// Next 4: Sample Rate
	let sample_rate = read_u32(reader)?;
	// Skip 6: Bytes/Sec (4), Block align (2)
	reader.seek_relative(6)?;
	// Next 2: Sample size in bits
	let sample_size = read_u16(reader)?;
	if chunk_size > 16 {
		// Skip 2: Extension Size (2)
		reader.seek_relative(2)?;
		if chunk_size > 18 {
			// Skip 6: Sample size in bits (2), Speaker positions (4), GUID (16)
			reader.seek_relative(6)?;
			// Next 2: Subformat
			wave_format = read_u16(reader)?;
			// Skip 14: Fixed string of the GUID for some reason
			reader.seek_relative(14)?;
		}
	}

	println!("{chunk_size}, {sample_rate}, {channels}, {wave_format}, {sample_size}");
	let sample_type = determine_sample_type(wave_format, sample_size)?;

	println!("{:?}", sample_type);

	Ok(WavFormat { channels, sample_rate, sample_type })
}

/// Read in all the chunks of a wav file
fn read_chunks<R: ReadSeek>(reader: &mut R) -> WavResult<MinWav> {
	// First 4 bytes need to be RIFF
	if !confirm_bytes(reader, RIFF_ID) {
		return Err(WavError::RIFFInvalid);
	}

	// UNUSED: Total file size
	reader.seek_relative(4)?;

	// Then needs say WAVE
	if !confirm_bytes(reader, WAVE_ID) {
		return Err(WavError::WAVEInvalid);
	}

	let mut format: WavFormat = Default::default();
	let mut data_buffer = vec![];

	let mut fmt_read = false;
	let mut data_read = false;

	let mut id_buf = [0; 4];
	while !data_read && reader.read_exact(&mut id_buf).is_ok() {
		if id_buf == FMT_ID {
			fmt_read = true;
			format = read_fmt_chunk(reader)?;
		} else if id_buf == DATA_ID {
			data_read = true;
			// First 4: length of the data buffer in bytes
			let buffer_size = read_u32(reader)?;
			data_buffer = vec![0; buffer_size as usize];
			reader.read_exact(&mut data_buffer)?;
		}
	}

	if !fmt_read { return Err(WavError::MissingFMT); }
	if !data_read { return Err(WavError::MissingData); }

	Ok(MinWav { data_buffer, format })
}

fn write_u16<W: Write>(writer: &mut W, val: u16) -> ioResult<()> {
	writer.write_all(&val.to_le_bytes())
}

fn write_u32<W: Write>(writer: &mut W, val: u32) -> ioResult<()> {
	writer.write_all(&val.to_le_bytes())
}

/// Writes all the required chunks in the extensible format
fn write_chunks<W: Write>(writer: &mut W, data: &MinWav) -> ioResult<()> {
	const GUID_ENDING: [u8; 14] = *b"\x00\x00\x00\x00\x10\x00\x80\x00\x00\xAA\x00\x38\x9B\x71";

	let (wave_format, sample_bits) = match data.format.sample_type {
		SampleType::I16 => (WaveFormat::PCM, 16),
		SampleType::I32 => (WaveFormat::PCM, 32),
		SampleType::F32 => (WaveFormat::IEEE_FLOAT, 32),
		SampleType::F64 => (WaveFormat::IEEE_FLOAT, 64),
	};

	let sample_bytes = (sample_bits as u32) / 8;

	// Starting RIFF tag
	writer.write_all(&RIFF_ID)?;

	// Next 4: Total file size: 60 + data size
	write_u32(writer, (60 + data.data_buffer.len()) as u32)?;

	// WAVE tag
	writer.write_all(&WAVE_ID)?;

	// Format ID Tag
	writer.write_all(&FMT_ID)?;
	// Next 4: Size of this chunk
	write_u32(writer, 40)?;
	// Next 2: WaveFormat
	write_u16(writer, WaveFormat::EXTENSIBLE as u16)?;
	// Next 2: # Channels
	write_u16(writer, data.format.channels)?;
	// Next 4: Sample Rate
	write_u32(writer, data.format.sample_rate)?;
	// Next 4: Bytes/Sec
	write_u32(writer, data.format.sample_rate * sample_bytes)?;
	// Next 2: Block align
	write_u16(writer, data.format.channels * (sample_bytes as u16))?;
	// Next 2: Sample size in bits
	write_u16(writer, sample_bits)?;
	// Next 2: Extension Size
	write_u16(writer, 22)?;
	// Next 2: Sample size in bits
	write_u16(writer, sample_bits)?;
	// Next 4: Speaker positions
	write_u32(writer, 0)?;
	// Next 2: Format
	write_u16(writer, wave_format as u16)?;
	// Next 14: Fixed GUID string
	writer.write_all(&GUID_ENDING)?;

	// fact chunk
	// Fact ID Tag
	writer.write_all(&FACT_ID)?;
	// Next 4: Chunk size
	write_u32(writer, 4)?;
	// Next 4: # Samples
	write_u32(writer, (data.data_buffer.len() as u32) / sample_bytes)?;

	// data chunk
	// Data ID Tag
	writer.write_all(&DATA_ID)?;
	// Next 4: Data length
	write_u32(writer, data.data_buffer.len() as u32)?;
	// Next N: Actual Data
	writer.write_all(&data.data_buffer)?;

	Ok(())
}

use conversion::ConvertTo;

/// Trait to mark a type as one of the available for being read/written in wav files
pub trait AudioSample: bytemuck::Pod { const WAV_TYPE: SampleType; }
impl AudioSample for i16 { const WAV_TYPE: SampleType = SampleType::I16; }
impl AudioSample for i32 { const WAV_TYPE: SampleType = SampleType::I32; }
impl AudioSample for f32 { const WAV_TYPE: SampleType = SampleType::F32; }
impl AudioSample for f64 { const WAV_TYPE: SampleType = SampleType::F64; }

/// Convert everything to mono by averaging
fn merge_channels<T: Default + Copy + ConvertTo<f64>>(samples: &Vec<T>, channels: usize) -> Vec<T>
where f64: ConvertTo<T> {
	samples.chunks_exact(channels).map(|raw| {
		let mut total: f64 = 0.0;
		for channel_sample in raw {
			total += channel_sample.convert_to();
		}
		(total / (channels as f64)).convert_to()
	}).collect()
}

/// Creates an iterator on `samples` of merged together tracks
///
/// Whatever T was going in, it gets converted to f64 and returned that way.
fn merge_iter<T: ConvertTo<f64>>(samples: &[T], channels: usize) -> impl Iterator<Item = f64> {
	samples.chunks_exact(channels).map(move |raw| {
		raw.iter().fold(0.0, |acc, val| {
			acc + val.convert_to()
		}) / (channels as f64)
	})
}

/// Convert from one sample type to another
pub fn convert_samples<SRC: ConvertTo<DST>, DST>(samples: &Vec<SRC>) -> Vec<DST> {
	samples.iter().map(ConvertTo::<DST>::convert_to).collect()
}

fn convert_iter<I: Iterator, DST>(iter: I) -> impl Iterator<Item = DST>
where I::Item: ConvertTo<DST> {
	iter.map(|sample| sample.convert_to())
}

/// Returns a tuple of the samples and the rate
///
/// ```rust
/// # use simple_wav::read;
/// # fn func() -> Result<(), Box<dyn std::error::Error>> {
/// let in_file = std::fs::File::open("/path/to/file.wav")?;
/// let reader = std::io::BufReader::new(in_file);
///	let (samples, rate) = read::<i16, _>(reader)?;
/// # Ok(())
/// # }
/// ```
#[allow(unused)]
pub fn read<S, R: ReadSeek>(mut reader: R) -> WavResult<(Vec<S>, usize)>
where
	i16: ConvertTo<S>,
	i32: ConvertTo<S>,
	f32: ConvertTo<S>,
	f64: ConvertTo<S>,
{
	let MinWav { data_buffer, format: WavFormat { sample_type, sample_rate, channels } } = read_chunks(&mut reader)?;

	let channels = channels as usize;
	let rate = sample_rate as usize;

	println!("Sampling Rate: {rate}");
	println!("Channels: {channels}");

	let samples = match sample_type {
		SampleType::I16 => {
			let temp = cast_slice::<u8, i16>(&data_buffer[..]);
			convert_iter(merge_iter(temp, channels)).collect()
			// let temp = merge_channels::<i16>(&temp.to_vec(), channels);
			// convert_samples(&temp)
		}
		SampleType::I32 => {
			let temp = cast_slice::<u8, i32>(&data_buffer[..]);
			let temp = merge_channels::<i32>(&temp.to_vec(), channels);
			convert_samples(&temp)
		}
		SampleType::F32 => {
			let temp = cast_slice::<u8, f32>(&data_buffer[..]);
			let temp = merge_channels::<f32>(&temp.to_vec(), channels);
			convert_samples(&temp)
		}
		SampleType::F64 => {
			let temp = cast_slice::<u8, f64>(&data_buffer[..]);
			let temp = merge_channels::<f64>(&temp.to_vec(), channels);
			convert_samples(&temp)
		}
	};

	Ok((samples, rate))
}

#[allow(unused)]
pub fn write<S: AudioSample, W: Write>(mut writer: W, samples: Vec<S>, rate: usize) -> ioResult<()> {
	let rate = rate as u32;

	let casted_samples = cast_slice::<S, u8>(&samples[..]);
	let wav_data = MinWav {
		data_buffer: casted_samples.to_vec(),
		format: WavFormat {
			sample_type: S::WAV_TYPE,
			sample_rate: rate,
			channels: 1
		}
	};

	write_chunks(&mut writer, &wav_data)?;

	Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
	use std::fs::File;
	use std::io::{BufReader, BufWriter};
	use std::path::Path;

	fn read_file<S, P: AsRef<Path>>(path: P) -> (Vec<S>, usize)
	where
		i16: ConvertTo<S>,
		i32: ConvertTo<S>,
		f32: ConvertTo<S>,
		f64: ConvertTo<S>
	{
		let in_file = std::fs::File::open(path).unwrap();
		let reader = BufReader::new(&in_file);
		read::<S, _>(reader).unwrap()
	}

	fn write_file<S: AudioSample, P: AsRef<Path>>(path: P, samples: Vec<S>, rate: usize) {
		let f = File::create(path).unwrap();
		write(BufWriter::new(f), samples, rate).unwrap();
	}

	#[test]
	fn saveload() {
		let (samples, rate) = read_file::<i16, _>("./audio/Test A.wav");
		write_file("./audio/Test A Out I16.wav", samples, rate);

		let (samples, rate) = read_file::<i32, _>("./audio/Test A.wav");
		write_file("./audio/Test A Out I32.wav", samples, rate);

		let (samples, rate) = read_file::<f32, _>("./audio/Test A.wav");
		write_file("./audio/Test A Out F32.wav", samples, rate);

		let (samples, rate) = read_file::<f64, _>("./audio/Test A.wav");
		write_file("./audio/Test A Out F64.wav", samples, rate);
	}

	// Test saturating casting
	#[test]
	fn casting() {
		let f: f64 = 1_000_000.0;
		let i: i16 = f as i16;
		assert_eq!(i, i16::MAX);
		let f: f64 = -1_000_000.0;
		let i: i16 = f as i16;
		assert_eq!(i, i16::MIN);
	}

	#[test]
	fn simple() {
		let in_file = std::fs::File::open("./audio/Test A.wav").unwrap();
		let (samples, rate) = read::<f64, _>(BufReader::new(&in_file)).unwrap();
		let f = File::create("./audio/Test A Simple Out.wav").unwrap();
		write(BufWriter::new(f), samples, rate).unwrap();
	}
}