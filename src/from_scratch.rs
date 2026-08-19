use std::fs::File;
use std::io::{BufWriter, Read, Seek, Write};
use std::path::Path;

use std::io::Result as ioResult;

use bytemuck::cast_slice;

/// Trait representing a type that can be used to read and seek.
trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SampleType { I16, I32, F32, F64 }

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
/// Uses assert for the comparison
///
/// Advances the reader forward by N
fn confirm_bytes<const N: usize>(reader: &mut Box<dyn ReadSeek>, target: [u8; N]) {
	let mut buf = [0; N];
	reader.read_exact(&mut buf);
	assert_eq!(buf, target);
}

/// Reads two bytes and converts them to a u16, assuming little endian
fn read_u16(reader: &mut Box<dyn ReadSeek>) -> ioResult<u16> {
	let mut buf = [0; 2];
	reader.read_exact(&mut buf)?;
	Ok(u16::from_le_bytes(buf))
}

/// Reads four bytes and converts them to a u32, assuming little endian
fn read_u32(reader: &mut Box<dyn ReadSeek>) -> ioResult<u32> {
	let mut buf = [0; 4];
	reader.read_exact(&mut buf)?;
	Ok(u32::from_le_bytes(buf))
}

/// Just the absolute minimum of important information in a wav file
struct MinWav {
	data_buffer: Vec<u8>, // Raw buffer of the samples
	sample_type: SampleType, // The sample type
	sample_rate: u32, // The sample rate
	channels: u16, // The number of channels
}

fn read_chunks(reader: &mut Box<dyn ReadSeek>) -> ioResult<MinWav> {
	// First 4 bytes need to be RIFF
	confirm_bytes(reader, RIFF_ID);

	// UNUSED: Total file size
	reader.seek_relative(4)?;

	// Then needs say WAVE
	confirm_bytes(reader, WAVE_ID);

	// Meaningless defaults for compiler reasons
	let mut channels = 0;
	let mut sample_rate = 0;
	let mut sample_type = SampleType::I16;
	let mut data_buffer = vec![];

	let mut fmt_read = false;
	let mut data_read = false;

	let mut id_buf = [0; 4];
	while !data_read && reader.read_exact(&mut id_buf).is_ok() {
		if id_buf == FMT_ID {
			fmt_read = true;
			// First 4: Size of this chunk
			let chunk_size = read_u32(reader)?;
			// Next 2: WaveFormat
			let mut wave_format = read_u16(reader)?;
			// Next 2: # Channels
			channels = read_u16(reader)?;
			// Next 4: Sample Rate
			sample_rate = read_u32(reader)?;
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
			sample_type =
				if wave_format == WaveFormat::PCM as u16 {
					if sample_size == 16 { SampleType::I16 }
					else if sample_size == 32 { SampleType::I32 }
					else { panic!("Invalid PCM sample width {}", sample_size) }
				} else if wave_format == WaveFormat::IEEE_FLOAT as u16 {
					if sample_size == 32 { SampleType::F32 }
					else if sample_size == 64 { SampleType::F64 }
					else { panic!("Invalid IEEE Float sample width {}", sample_size) }
				} else {
					panic!("Unsupported Format {}", wave_format)
				};

			println!("{:?}", sample_type);
		} else if id_buf == DATA_ID {
			data_read = true;
			// First 4: length of the data buffer in bytes
			let buffer_size = read_u32(reader)?;
			data_buffer = vec![0; buffer_size as usize];
			reader.read_exact(&mut data_buffer)?;
		}
	}

	if !fmt_read { panic!("File didn't have a format chunk!") }
	if !data_read { panic!("File didn't have a data chunk!") }

	Ok(MinWav{ data_buffer, sample_type, sample_rate, channels })
}

fn write_u16(writer: &mut Box<dyn Write>, val: u16) -> ioResult<()> {
	writer.write_all(&val.to_le_bytes())
}

fn write_u32(writer: &mut Box<dyn Write>, val: u32) -> ioResult<()> {
	writer.write_all(&val.to_le_bytes())
}

fn write_ext_chunks(writer: &mut Box<dyn Write>, data: &MinWav) -> ioResult<()> {
	const GUID_ENDING: [u8; 14] = *b"\x00\x00\x00\x00\x10\x00\x80\x00\x00\xAA\x00\x38\x9B\x71";

	let (wave_format, sample_bits) = match data.sample_type {
		SampleType::I16 => (WaveFormat::PCM, 16),
		SampleType::I32 => (WaveFormat::PCM, 32),
		SampleType::F32 => (WaveFormat::IEEE_FLOAT, 32),
		SampleType::F64 => (WaveFormat::IEEE_FLOAT, 64),
	};

	let sample_bytes = (sample_bits as u32) / 8;

	// Total file size: 60 + data size
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
	write_u16(writer, data.channels)?;
	// Next 4: Sample Rate
	write_u32(writer, data.sample_rate)?;
	// Next 4: Bytes/Sec
	write_u32(writer, data.sample_rate * sample_bytes)?;
	// Next 2: Block align
	write_u16(writer, data.channels * (sample_bytes as u16))?;
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

fn write_chunks(writer: &mut Box<dyn Write>, data: &MinWav) -> ioResult<()> {
	// Starting RIFF tag
	writer.write_all(&RIFF_ID)?;

	write_ext_chunks(writer, data)?;

	Ok(())
}

use conversion::ConvertTo;

/// Trait to mark a type as one of the available for being read/written in wav files
pub trait AudioSample:
	bytemuck::Pod + Default +
	ConvertTo<i16> + ConvertTo<i32> + ConvertTo<f32> + ConvertTo<f64>
{
	const WAV_TYPE: SampleType;
}
impl AudioSample for i16 { const WAV_TYPE: SampleType = SampleType::I16; }
impl AudioSample for i32 { const WAV_TYPE: SampleType = SampleType::I32; }
impl AudioSample for f32 { const WAV_TYPE: SampleType = SampleType::F32; }
impl AudioSample for f64 { const WAV_TYPE: SampleType = SampleType::F64; }

fn merge_channels<T: Default + Copy + ConvertTo<f64>>(samples: &Vec<T>, channels: usize) -> Vec<T>
where f64: ConvertTo<T> {
	let mut arr: Vec<T> = vec![Default::default(); samples.len() / channels];
	// Convert everything to mono by averaging
	for (idx, raw) in samples.chunks_exact(channels).enumerate() {
		// Use f64 as an intermediate because it will work with float and int targets
		// We do technically lose a little bit of precision theoretically, but I don't think it will matter
		let mut total: f64 = 0.0;
		for channel_sample in raw {
			total += channel_sample.convert_to();
		}

		arr[idx] = (total / (channels as f64)).convert_to();
	}
	arr
}

/// Convert from one sample type to another
pub fn convert_samples<SRC: ConvertTo<DST>, DST>(samples: &Vec<SRC>) -> Vec<DST> {
	samples.iter().map(ConvertTo::<DST>::convert_to).collect()
}

/// Returns a tuple of the samples and the rate
#[allow(unused)]
pub fn read<S: Default + Copy + ConvertTo<f64>, P: AsRef<Path>>(path: P) -> ioResult<(Vec<S>, usize)>
where
	i16: ConvertTo<S>,
	i32: ConvertTo<S>,
	f32: ConvertTo<S>,
	f64: ConvertTo<S>,
{
	let f = std::fs::File::open(path).unwrap();
	let mut reader: Box<dyn ReadSeek> = Box::new(std::io::BufReader::new(f));
	let MinWav { data_buffer, sample_type, sample_rate, channels } = read_chunks(&mut reader)?;

	let channels = channels as usize;
	let rate = sample_rate as usize;

	println!("Sampling Rate: {rate}");
	println!("Channels: {channels}");

	let samples = match sample_type {
		SampleType::I16 => {
			let temp = cast_slice::<u8, i16>(&data_buffer[..]);
			convert_samples(&temp.to_vec())
		}
		SampleType::I32 => {
			let temp = cast_slice::<u8, i32>(&data_buffer[..]);
			convert_samples(&temp.to_vec())
		}
		SampleType::F32 => {
			let temp = cast_slice::<u8, f32>(&data_buffer[..]);
			convert_samples(&temp.to_vec())
		}
		SampleType::F64 => {
			let temp = cast_slice::<u8, f64>(&data_buffer[..]);
			convert_samples(&temp.to_vec())
		}
	};

	Ok((merge_channels(&samples, channels), rate))
}

#[allow(unused)]
pub fn write<S: AudioSample, P: AsRef<Path>>(samples: Vec<S>, rate: usize, path: P) -> ioResult<()>{
	let rate = rate as u32;

	let casted_samples = cast_slice::<S, u8>(&samples[..]);
	let wav_data = MinWav {
		data_buffer: casted_samples.to_vec(),
		sample_type: S::WAV_TYPE,
		sample_rate: rate,
		channels: 1
	};

	let f = File::create(&path).unwrap();
	let mut writer: Box<dyn Write> = Box::new(BufWriter::new(f));

	write_chunks(&mut writer, &wav_data)?;

	println!("Finished writing to file: {:?}", path.as_ref());

	Ok(())
}