use std::fs::File;
use std::io::{BufWriter, Read, Seek, Write};
use std::path::Path;

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
const DATA_ID: [u8; 4] = *b"data";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
#[allow(non_camel_case_types)]
enum WaveFormat {
	PCM = 0x0001,
	IEEE_FLOAT = 0x0003,
	ALAW = 0x0006,
	MULAW = 0x0007,
	EXTENSIBLE = 0xFFFE
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
fn read_u16(reader: &mut Box<dyn ReadSeek>) -> u16 {
	let mut buf = [0; 2];
	reader.read_exact(&mut buf);
	u16::from_le_bytes(buf)
}

/// Reads four bytes and converts them to a u32, assuming little endian
fn read_u32(reader: &mut Box<dyn ReadSeek>) -> u32 {
	let mut buf = [0; 4];
	reader.read_exact(&mut buf);
	u32::from_le_bytes(buf)
}

fn read_chunks(reader: &mut Box<dyn ReadSeek>) -> (
	Vec<u8>, // Raw buffer of the samples
	SampleType, // The sample type
	u32, // The sample rate
	u16, // The number of channels
) {
	// First 4 bytes need to be RIFF
	confirm_bytes(reader, RIFF_ID);

	// UNUSED: Total file size
	reader.seek_relative(4);

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
			// Reusable buffers
			let mut buf4 = [0; 4];
			let mut buf2 = [0; 2];
			// First 4: Size of this chunk
			reader.read_exact(&mut buf4);
			let chunk_size = u32::from_le_bytes(buf4);
			// Next 2: WaveFormat
			reader.read_exact(&mut buf2);
			// This is Native Endian because we're comparing to hex literals rather than numbers
			let mut wave_format = u16::from_ne_bytes(buf2);
			// Next 2: # Channels
			reader.read_exact(&mut buf2);
			channels = u16::from_le_bytes(buf2);
			// Next 4: Sample Rate
			reader.read_exact(&mut buf4);
			sample_rate = u32::from_le_bytes(buf4);
			// Skip 6: Bytes/Sec (4), Block align (2)
			reader.seek_relative(6);
			// Next 2: Sample size in bits
			reader.read_exact(&mut buf2);
			let sample_size = u16::from_le_bytes(buf2);
			if chunk_size > 16 {
				// Skip 2: Extension Size (2)
				reader.seek_relative(2);
				if chunk_size > 18 {
					// Skip 6: Sample size in bits (2), Speaker positions (4), GUID (16)
					reader.seek_relative(6);
					// Next 2: Subformat
					reader.read_exact(&mut buf2);
					// This is Native Endian because we're comparing to hex literals rather than numbers
					wave_format = u16::from_ne_bytes(buf2);
					// Skip 14: Fixed string of the GUID for some reason
					reader.seek_relative(14);
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
					panic!("Unsupported Format!")
				};

			println!("{:?}", sample_type);
		} else if id_buf == DATA_ID {
			data_read = true;
			// First 4: length of the data buffer in bytes
			let mut buf = [0; 4];
			reader.read_exact(&mut buf);
			let buffer_size = u32::from_le_bytes(buf);
			data_buffer = vec![0; buffer_size as usize];
			reader.read_exact(&mut data_buffer);
		}
	}

	if !fmt_read { panic!("File didn't have a format chunk!") }
	if !data_read { panic!("File didn't have a data chunk!") }

	(data_buffer, sample_type, sample_rate, channels)
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

/// Converts a raw byte vec into a vector of samples of the target type
/// Using the passed generic as an intermediate
fn convert_buffer<T: AudioSample, const N: usize>(buf: &Vec<u8>, channels: usize) -> Vec<T>
where
	[u8; N]: ConvertTo<T>, // Make sure we have the right N to convert to the type
	f64: ConvertTo<T>,
{
	let (bytes, _) = buf.as_chunks::<N>();
	let mut arr: Vec<T> = vec![Default::default(); bytes.len() / channels];
	// Convert everything to mono by averaging
	for (idx, raw) in bytes.chunks_exact(channels).enumerate() {
		// Use f64 as an intermediate because it will work with float and int targets
		// We do technically lose a little bit of precision theoretically, but I don't think it will matter
		let mut total: f64 = 0.0;
		for channel_sample in raw {
			// Convert it to T
			let temp: T = (*channel_sample).convert_to();
			total += <T as ConvertTo<f64>>::convert_to(&temp);
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
pub fn read<S, P: AsRef<Path>>(path: P) -> (Vec<S>, usize)
where
	i16: ConvertTo<S>,
	i32: ConvertTo<S>,
	f32: ConvertTo<S>,
	f64: ConvertTo<S>,
{
	let f = std::fs::File::open(path).unwrap();
	let mut reader: Box<dyn ReadSeek> = Box::new(std::io::BufReader::new(f));
	let (buf, wav_type, rate, channels) = read_chunks(&mut reader);

	let channels = channels as usize;
	let rate = rate as usize;

	println!("Sampling Rate: {rate}");
	println!("Channels: {channels}");

	let samples = match wav_type {
		SampleType::I16 => {
			let temp = convert_buffer::<i16, _>(&buf, channels);
			convert_samples(&temp)
		}
		SampleType::I32 => {
			let temp = convert_buffer::<i32, _>(&buf, channels);
			convert_samples(&temp)
		}
		SampleType::F32 => {
			let temp = convert_buffer::<f32, _>(&buf, channels);
			convert_samples(&temp)
		}
		SampleType::F64 => {
			let temp = convert_buffer::<f64, _>(&buf, channels);
			convert_samples(&temp)
		}
	};

	(samples, rate)
}

// pub fn write<S: AudioSample, P: AsRef<Path>>(samples: Vec<S>, rate: usize, path: P) {
// 	let wav_header = header::WavHeader::new_header(
// 		rate as i32,
// 		1,
// 		samples.len(),
// 		S::WAV_TYPE
// 	);

// 	let f = File::create(&path).unwrap();
// 	let mut buf_writer: BufWriter<File> = BufWriter::new(f);

// 	// TODO: Base/CB/Extended Option
// 	let header_bytes = wav_header.as_extended_bytes();
// 	// let header_bytes = wav_header.as_base_bytes();
// 	buf_writer.write_all(&header_bytes).unwrap();

// 	let sample_bytes = cast_slice::<S, u8>(&samples[..]);
// 	let data_size_bytes = sample_bytes.len() as u32;
// 	buf_writer.write_all(&chunks::DATA).unwrap();
// 	buf_writer.write_all(&data_size_bytes.to_ne_bytes()).unwrap(); // write the data size
// 	buf_writer.write_all(&sample_bytes).unwrap(); // write the data
// 	println!("Finished writing to file: {:?}", path.as_ref());
// }