// Originally ripped near directly from https://github.com/jmg049/wavers/
// """Cleaned up""" (see: made worse and less robust in nearly every way in the name of simplicity)
// Basically I wanted something super simple to read and write audio just with arrays of samples

mod from_scratch;

use std::fs::File;
use std::io::{BufWriter, Read, Seek, Write};
use std::path::Path;

use bytemuck::cast_slice;

/// Trait representing a type that can be used to read and seek.
trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

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
			i16::from_ne_bytes(*self)
		}
	}
	impl ConvertTo<i32> for [u8; 4] {
		fn convert_to(&self) -> i32 {
			i32::from_ne_bytes(*self)
		}
	}
	impl ConvertTo<f32> for [u8; 4] {
		fn convert_to(&self) -> f32 {
			f32::from_ne_bytes(*self)
		}
	}
	impl ConvertTo<f64> for [u8; 8] {
		fn convert_to(&self) -> f64 {
			f64::from_ne_bytes(*self)
		}
	}
}

/// Module containing the ``Chunk`` trait and the constants relating to the different chunks in a wav file.
mod chunks {
	use std::{
		fmt::Display,
		io::SeekFrom,
	};
	use super::{
		header::HeaderChunkInfo,
		ReadSeek,
		wav_type::FormatCode,
	};

	// Chunk IDs
	pub const RIFF: [u8; 4] = *b"RIFF";
	pub const WAVE: [u8; 4] = *b"WAVE";
	pub const DATA: [u8; 4] = *b"data";
	pub const FMT: [u8; 4] = *b"fmt ";
	pub const FACT: [u8; 4] = *b"fact";

	/// A trait representing a chunk in a wav file.
	/// Allows for the common creation of several chunks that are found in a wav file.
	/// Requires them to implement common decoding and encoding methods.
	pub trait Chunk: Display {
		fn size(&self) -> u32;
		fn as_bytes(&self) -> Box<[u8]>;
		fn from_reader(reader: &mut Box<dyn ReadSeek>, info: &HeaderChunkInfo) -> Self
			where Self: Sized;
	}

	/// Read a chunk from a wav file.
	pub fn read_chunk<T: Chunk>(
		reader: &mut Box<dyn ReadSeek>,
		info: &HeaderChunkInfo,
	) -> T {
		T::from_reader(reader, info)
	}

	/// The fmt chunk of a wav file. This chunk contains information about the format of the audio data.
	pub const FMT_SIZE_BASE_SIZE: usize = 16; // Standard wav file format size
	pub const FMT_CB_SIZE: usize = 18; // An extended Format chunk is used for non-PCM data. The cbSize field gives the size of the extension. (0 or 22)
	pub const FMT_SIZE_EXTENDED_SIZE: usize = 40; // CB_SIZE + 22 (2 bytes valid_bits_per_sample, 4 byte channel_mask, 16(2+14) byte sub_format)

	const EXTENDED_FMT_GUID: [u8; 14] = *b"\x00\x00\x00\x00\x10\x00\x80\x00\x00\xAA\x00\x38\x9B\x71";
	pub const EXT_FORMAT_CODE: u16 = 0xFFFE;

	/// An enum used to represent the two possible sizes of the extensible format chunk.
	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
	#[repr(u16)]
	pub enum CbSize {
		Base = 0,
		Extended = 22,
	}

	impl Display for CbSize {
		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
			match self {
				CbSize::Base => write!(f, "Base (0)"),
				CbSize::Extended => write!(f, "Extended (22)"),
			}
		}
	}

	/// The extended format chunk of a wav file. This chunk contains additional information about the format of the audio data.
	/// This chunk is present when the format code is set to WAVE_FORMAT_EXTENSIBLE.
	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
	#[repr(C)]
	pub struct ExtFmtChunkInfo {
		cb_size: CbSize,
		valid_bits_per_sample: u16,
		channel_mask: u32,
		sub_format: FormatCode,
		guid: [u8; 14],
	}

	impl ExtFmtChunkInfo {
		pub const fn new(
			cb_size: CbSize,
			valid_bits_per_sample: u16,
			channel_mask: u32,
			sub_format: FormatCode,
		) -> Self {
			ExtFmtChunkInfo {
				cb_size,
				valid_bits_per_sample,
				channel_mask,
				sub_format,
				guid: EXTENDED_FMT_GUID,
			}
		}

		pub fn as_bytes(&self) -> [u8; 24] {
			let mut bytes = [0; 24];
			let cb_size = self.cb_size as u16;

			bytes[0..2].copy_from_slice(&cb_size.to_ne_bytes());
			bytes[2..4].copy_from_slice(&self.valid_bits_per_sample.to_ne_bytes());
			bytes[4..8].copy_from_slice(&self.channel_mask.to_ne_bytes());
			bytes[8..10].copy_from_slice(&self.sub_format.to_ne_bytes());
			bytes[10..24].copy_from_slice(&self.guid);
			bytes
		}

		pub const fn default_bytes(
			format_code: FormatCode,
			bits_per_sample: u16,
		) -> [u8; std::mem::size_of::<Self>()] {
			let mut bytes = [0; std::mem::size_of::<Self>()];

			let cb_size = CbSize::Base as u16;
			let cb_size_bytes = cb_size.to_ne_bytes();
			let mut i = 0;
			while i < cb_size_bytes.len() {
				bytes[i] = cb_size_bytes[i];
				i += 1;
			}

			let valid_bits_per_sample_bytes = bits_per_sample.to_ne_bytes();
			let mut i = 0;
			while i < valid_bits_per_sample_bytes.len() {
				bytes[i + 2] = valid_bits_per_sample_bytes[i];
				i += 1;
			}

			let channel_mask: i32 = 0;
			let channel_mask_bytes = channel_mask.to_ne_bytes();
			let mut i = 0;
			while i < channel_mask_bytes.len() {
				bytes[i + 4] = channel_mask_bytes[i];
				i += 1;
			}

			let sub_format = format_code;
			let sub_format_bytes = sub_format.to_ne_bytes();
			let mut i = 0;
			while i < sub_format_bytes.len() {
				bytes[i + 8] = sub_format_bytes[i];
				i += 1;
			}

			let guid = EXTENDED_FMT_GUID;
			let mut i = 0;
			while i < guid.len() {
				bytes[i + 10] = guid[i];
				i += 1;
			}

			bytes
		}
	}

	impl Default for ExtFmtChunkInfo {
		fn default() -> Self {
			Self {
				cb_size: CbSize::Base,
				valid_bits_per_sample: (std::mem::size_of::<i16>() * 8) as u16,
				channel_mask: 0,
				sub_format: FormatCode::WAV_FORMAT_PCM,
				guid: EXTENDED_FMT_GUID,
			}
		}
	}

	impl Display for ExtFmtChunkInfo {
		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
			write!(f, "\tExtended Format Chunk\n")?;
			write!(f, "\t\tCB Size: {}\n", self.cb_size)?;
			write!(
				f,
				"\t\tValid Bits Per Sample: {}\n",
				self.valid_bits_per_sample
			)?;
			write!(f, "\t\tChannel Mask: {}\n", self.channel_mask)?;
			write!(f, "\t\tSub Format: {}\n", self.sub_format)?;
			write!(f, "\t\tGUID: {:?}\n", self.guid)
		}
	}

	/// The format chunk of a wav file. This chunk contains information about the format of the audio data.
	/// This chunk must be present in a wav file.
	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
	#[repr(C)]
	pub struct FmtChunk {
		/// Format of the audio data. 1 for PCM, 3 for IEEE float.
		pub format: FormatCode,
		/// Number of channels in the audio data.
		pub channels: u16,
		/// Sample rate of the audio data.
		pub sample_rate: i32,
		/// Byte rate of the audio data.
		pub byte_rate: i32,
		/// Block align of the audio data.
		pub block_align: u16,
		/// Bits per sample of the audio data.
		pub bits_per_sample: u16,
		pub ext_fmt_chunk: ExtFmtChunkInfo,
	}

	impl FmtChunk {
		/// Constructs a new FmtChunk using the provided format, number of channels, sample rate and bits per sample.
		/// The remaining fields are calculating using these arguments
		pub fn new(
			format: FormatCode,
			channels: u16,
			sample_rate: i32,
			bits_per_sample: u16,
			ext_fmt_chunk: ExtFmtChunkInfo,
		) -> Self {
			let block_align = (channels * bits_per_sample) / 8;
			let byte_rate = sample_rate * (block_align as i32);
			FmtChunk {
				format,
				channels,
				sample_rate,
				byte_rate,
				block_align,
				bits_per_sample,
				ext_fmt_chunk,
			}
		}

		fn is_extended_format(&self) -> bool {
			self.format as u16 == EXT_FORMAT_CODE
		}

		pub fn format(&self) -> FormatCode {
			match self.is_extended_format() {
				true => self.ext_fmt_chunk.sub_format,
				false => self.format,
			}
		}

		/// Get the format data as a set of bytes in the standard WAV format
		fn base_bytes(&self) -> [u8; FMT_SIZE_BASE_SIZE] {
			let mut bytes = [0; FMT_SIZE_BASE_SIZE];
			bytes[0..2].copy_from_slice(&self.format.to_ne_bytes());
			bytes[2..4].copy_from_slice(&self.channels.to_ne_bytes());
			bytes[4..8].copy_from_slice(&self.sample_rate.to_ne_bytes());
			bytes[8..12].copy_from_slice(&self.byte_rate.to_ne_bytes());
			bytes[12..14].copy_from_slice(&self.block_align.to_ne_bytes());
			bytes[14..16].copy_from_slice(&self.bits_per_sample.to_ne_bytes());
			bytes
		}

		fn cb_bytes(&self) -> [u8; FMT_CB_SIZE] {
			let mut bytes = [0; FMT_CB_SIZE];
			bytes[0..16].copy_from_slice(&self.base_bytes());
			let cb: u16 = self.ext_fmt_chunk.cb_size as u16;
			bytes[16..18].copy_from_slice(&cb.to_ne_bytes());
			bytes
		}

		fn extended_bytes(&self) -> [u8; FMT_SIZE_EXTENDED_SIZE] {
			let mut bytes = [0; FMT_SIZE_EXTENDED_SIZE];
			bytes[0..16].copy_from_slice(&self.base_bytes());
			bytes[16..40].copy_from_slice(&self.ext_fmt_chunk.as_bytes());
			bytes
		}

		/// Create a format data struct out of a list of bytes
		fn from_base_bytes(bytes: [u8; FMT_SIZE_BASE_SIZE]) -> Self {
			let mut buf: [u8; FMT_SIZE_EXTENDED_SIZE] = [0; FMT_SIZE_EXTENDED_SIZE];

			let mut info_buf: [u8; 2] = [0; 2];
			info_buf.copy_from_slice(&bytes[0..2]);
			let format = FormatCode::try_from(u16::from_ne_bytes(info_buf)).unwrap();

			info_buf.copy_from_slice(&bytes[14..16]);
			let bits_per_sample = u16::from_ne_bytes(info_buf);

			buf[0..FMT_SIZE_BASE_SIZE].copy_from_slice(&bytes);
			buf[FMT_SIZE_BASE_SIZE..FMT_SIZE_EXTENDED_SIZE]
				.copy_from_slice(&ExtFmtChunkInfo::default_bytes(format, bits_per_sample));
			FmtChunk::from_extended_bytes(buf)
		}

		fn from_cb_bytes(bytes: [u8; FMT_CB_SIZE]) -> Self {
			let mut buf: [u8; FMT_SIZE_EXTENDED_SIZE] = [0; FMT_SIZE_EXTENDED_SIZE];
			buf[0..FMT_CB_SIZE].copy_from_slice(&bytes);
			let mut info_buf: [u8; 2] = [0; 2];

			info_buf.copy_from_slice(&bytes[0..2]);

			let format = FormatCode::try_from(u16::from_ne_bytes(info_buf)).unwrap();

			info_buf.copy_from_slice(&bytes[14..16]);
			let bits_per_sample = u16::from_ne_bytes(info_buf);
			buf[FMT_CB_SIZE - 2..FMT_SIZE_EXTENDED_SIZE]
				.copy_from_slice(&ExtFmtChunkInfo::default_bytes(format, bits_per_sample));
			FmtChunk::from_extended_bytes(buf)
		}

		fn from_extended_bytes(bytes: [u8; FMT_SIZE_EXTENDED_SIZE]) -> Self {
			unsafe { std::mem::transmute_copy(&bytes) }
		}
	}

	impl Chunk for FmtChunk {
		fn size(&self) -> u32 {
			match self.is_extended_format() {
				true => match self.ext_fmt_chunk.cb_size {
					CbSize::Base => FMT_CB_SIZE as u32,
					CbSize::Extended => FMT_SIZE_EXTENDED_SIZE as u32,
				},
				false => FMT_SIZE_BASE_SIZE as u32,
			}
		}

		fn as_bytes(&self) -> Box<[u8]> {
			match self.is_extended_format() {
				true => match self.ext_fmt_chunk.cb_size {
					CbSize::Base => {
						let mut bytes = [0; FMT_CB_SIZE + 8];
						bytes[0..4].copy_from_slice(&FMT);
						bytes[4..8].copy_from_slice(&self.size().to_ne_bytes());
						bytes[8..FMT_CB_SIZE + 8].copy_from_slice(&self.cb_bytes());
						Box::new(bytes)
					}
					CbSize::Extended => {
						let mut bytes = [0; FMT_SIZE_EXTENDED_SIZE + 8];
						bytes[0..4].copy_from_slice(&FMT);
						bytes[4..8].copy_from_slice(&self.size().to_ne_bytes());
						bytes[8..FMT_SIZE_EXTENDED_SIZE + 8].copy_from_slice(&self.extended_bytes());
						Box::new(bytes)
					}
				},
				false => {
					let mut bytes = [0; FMT_SIZE_BASE_SIZE + 8];
					bytes[0..4].copy_from_slice(&FMT);
					bytes[4..8].copy_from_slice(&self.size().to_ne_bytes());
					bytes[8..FMT_SIZE_BASE_SIZE + 8].copy_from_slice(&self.base_bytes());
					Box::new(bytes)
				}
			}
		}

		fn from_reader(reader: &mut Box<dyn ReadSeek>, info: &HeaderChunkInfo) -> Self {
			let offset = info.offset as u64 + 8;
			reader.seek(SeekFrom::Start(offset)).unwrap();
			let mut fmt_code_buf: [u8; 2] = [0; 2];
			reader.read_exact(&mut fmt_code_buf).unwrap();

			// move the reader back two bytes so that we can decode the fmt format code again
			reader.seek_relative(-2).unwrap();

			let main_format_code = FormatCode::try_from(u16::from_ne_bytes(fmt_code_buf)).unwrap();
			let total_size_in_bytes = info.size as usize;
			let fmt_chunk: FmtChunk = match main_format_code {
				FormatCode::WAV_FORMAT_PCM => {
					let mut fmt_buf = [0; FMT_SIZE_BASE_SIZE];
					reader.read_exact(&mut fmt_buf).unwrap();
					FmtChunk::from_base_bytes(fmt_buf)
				}
				FormatCode::WAV_FORMAT_IEEE_FLOAT => {
					// In theory, the below is the correct approach for non-PCM formats, but so far with testing, it seems that the fmt chunk is always 16 bytes long when the format is set to 3.
					// let mut fmt_buf: [u8; FMT_CB_SIZE] = [0; FMT_CB_SIZE];
					let mut fmt_buf = [0; FMT_SIZE_BASE_SIZE];
					reader.read_exact(&mut fmt_buf).unwrap();
					FmtChunk::from_base_bytes(fmt_buf)
				}
				FormatCode::WAVE_FORMAT_ALAW => todo!(),
				FormatCode::WAVE_FORMAT_MULAW => todo!(),
				FormatCode::WAVE_FORMAT_EXTENSIBLE => match total_size_in_bytes {
					FMT_CB_SIZE => {
						let mut fmt_buf = [0; FMT_CB_SIZE];
						reader.read_exact(&mut fmt_buf).unwrap();
						FmtChunk::from_cb_bytes(fmt_buf)
					}
					FMT_SIZE_EXTENDED_SIZE => {
						let mut fmt_buf = [0; FMT_SIZE_EXTENDED_SIZE];
						reader.read_exact(&mut fmt_buf).unwrap();
						FmtChunk::from_extended_bytes(fmt_buf)
					}
					_ => panic!("Invalid format chunk size {}", total_size_in_bytes),
				},
			};

			fmt_chunk
		}
	}

	impl Display for FmtChunk {
		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
			write!(f, "FmtChunk\n")?;
			write!(f, "\tFormat: {}\n", self.format)?;
			write!(f, "\tChannels: {}\n", self.channels)?;
			write!(f, "\tSample Rate: {}\n", self.sample_rate)?;
			write!(f, "\tByte Rate: {}\n", self.byte_rate)?;
			write!(f, "\tBlock Align: {}\n", self.block_align)?;
			write!(f, "\tBits Per Sample: {}\n", self.bits_per_sample)?;
			write!(f, "{}", self.ext_fmt_chunk)
		}
	}
}

/// Module for the WavType enum and related functions.
mod wav_type {
	use std::fmt::Display;

	const PCM_16_BITS: u16 = (std::mem::size_of::<i16>() * 8) as u16;
	const PCM_32_BITS: u16 = (std::mem::size_of::<i32>() * 8) as u16;
	const FLOAT_32_BITS: u16 = (std::mem::size_of::<f32>() * 8) as u16;
	const FLOAT_64_BITS: u16 = (std::mem::size_of::<f64>() * 8) as u16;

	/// An enum representing some of the format codes in the wav file format.
	#[allow(non_camel_case_types)]
	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
	pub enum FormatCode {
		WAV_FORMAT_PCM = 0x0001,
		WAV_FORMAT_IEEE_FLOAT = 0x0003,
		WAVE_FORMAT_ALAW = 0x0006,
		WAVE_FORMAT_MULAW = 0x0007,
		WAVE_FORMAT_EXTENSIBLE = 0xFFFE,
	}

	impl FormatCode {
		/// Convert the format code to a native endian byte array.
		pub const fn to_ne_bytes(self) -> [u8; 2] {
			(self as u16).to_ne_bytes()
		}
	}

	impl Display for FormatCode {
		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
			match self {
				FormatCode::WAV_FORMAT_PCM => write!(f, "WAV_FORMAT_PCM"),
				FormatCode::WAV_FORMAT_IEEE_FLOAT => write!(f, "WAV_FORMAT_IEEE_FLOAT"),
				FormatCode::WAVE_FORMAT_ALAW => write!(f, "WAVE_FORMAT_ALAW"),
				FormatCode::WAVE_FORMAT_MULAW => write!(f, "WAVE_FORMAT_MULAW"),
				FormatCode::WAVE_FORMAT_EXTENSIBLE => write!(f, "WAV_EXTENSIBLE_FORMAT"),
			}
		}
	}

	impl TryFrom<u16> for FormatCode {
		type Error = String;

		fn try_from(value: u16) -> Result<Self, String> {
			match value {
				0x0001 => Ok(FormatCode::WAV_FORMAT_PCM),
				0x0003 => Ok(FormatCode::WAV_FORMAT_IEEE_FLOAT),
				0x0006 => Ok(FormatCode::WAVE_FORMAT_ALAW),
				0x0007 => Ok(FormatCode::WAVE_FORMAT_MULAW),
				0xFFFE => Ok(FormatCode::WAVE_FORMAT_EXTENSIBLE),
				_ => Err(format!("Value {} is not a valid format code", value)),
			}
		}
	}

	/// Enum representing the encoding of a wav file.
	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
	pub enum WavType {
		Pcm16, // PCM_16
		Pcm32, // PCM_32
		Float32, // IEEE_FLOAT_32
		Float64, // IEEE_FLOAT_64
		EPcm16, // EXTENSIBLE_PCM_16
		EPcm32, // EXTENSIBLE_PCM_32
		EFloat32, // EXTENSIBLE_IEEE_FLOAT_32
		EFloat64, // EXTENSIBLE_IEEE_FLOAT_64
	}

	impl WavType {
		/// Converts the WavType to the number of bits per sample.
		pub const fn n_bits(&self) -> u16 {
			match self {
				WavType::Pcm16 | WavType::EPcm16 => PCM_16_BITS,
				WavType::Pcm32 | WavType::EPcm32 => PCM_32_BITS,
				WavType::Float32 | WavType::EFloat32 => FLOAT_32_BITS,
				WavType::Float64 | WavType::EFloat64 => FLOAT_64_BITS,
			}
		}

		/// Converts the WavType to the number of bytes per sample.
		pub const fn n_bytes(&self) -> usize {
			match self {
				WavType::Pcm16 | WavType::EPcm16 => std::mem::size_of::<i16>(),
				WavType::Pcm32 | WavType::EPcm32 => std::mem::size_of::<i32>(),
				WavType::Float32 | WavType::EFloat32 => std::mem::size_of::<f32>(),
				WavType::Float64 | WavType::EFloat64 => std::mem::size_of::<f64>(),
			}
		}
	}

	/// Converts a tuple of format codes and bits per sample to a WavType.
	pub const fn format_info_to_wav_type(info: (FormatCode, u16, FormatCode)) -> WavType {
		match info {
			(FormatCode::WAV_FORMAT_PCM, PCM_16_BITS, _) => WavType::Pcm16,
			(FormatCode::WAV_FORMAT_PCM, PCM_32_BITS, _) => WavType::Pcm32,
			(FormatCode::WAV_FORMAT_IEEE_FLOAT, FLOAT_32_BITS, _) => WavType::Float32,
			(FormatCode::WAV_FORMAT_IEEE_FLOAT, FLOAT_64_BITS, _) => WavType::Float64,
			(FormatCode::WAVE_FORMAT_EXTENSIBLE, PCM_16_BITS, FormatCode::WAV_FORMAT_PCM) => {
				WavType::EPcm16
			}
			(FormatCode::WAVE_FORMAT_EXTENSIBLE, PCM_32_BITS, FormatCode::WAV_FORMAT_PCM) => {
				WavType::EPcm32
			}
			(FormatCode::WAVE_FORMAT_EXTENSIBLE, FLOAT_32_BITS, FormatCode::WAV_FORMAT_IEEE_FLOAT) => {
				WavType::EFloat32
			}
			(FormatCode::WAVE_FORMAT_EXTENSIBLE, FLOAT_64_BITS, FormatCode::WAV_FORMAT_IEEE_FLOAT) => {
				WavType::EFloat64
			}
			_ => {
				panic!("The uhhhh info was wrong?");
			}
		}
	}
}

mod header {
	use std::{
		collections::HashMap,
		fmt::Debug,
		io::SeekFrom,
	};

	use super::{
		chunks::{
			ExtFmtChunkInfo, CbSize, FMT_SIZE_BASE_SIZE, FMT_SIZE_EXTENDED_SIZE,
			read_chunk, Chunk, FmtChunk, DATA, FMT, RIFF, WAVE,
		},
		wav_type::{format_info_to_wav_type, FormatCode, WavType},
	};

	const _RIFF_SIZE: usize = 4; // do not count the size or RIFF id, only the WAVE id. The other chunks will be used for the remaining size
	const HEADER_FMT_BASE_SIZE: usize = 36;
	const HEADER_FMT_EXTENDED_SIZE: usize = 60;

	/// Struct representing the information of a wav file.
	#[derive(Debug, Clone, PartialEq, Eq)]
	pub struct WavInfo {
		pub wav_type: WavType, // the type of the wav file
		pub wav_header: WavHeader,
	}

	/// A struct used to store the offset and size of a chunk
	#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
	pub struct HeaderChunkInfo {
		pub(crate) offset: usize,
		pub(crate) size: u32,
	}

	impl HeaderChunkInfo {
		/// Constructs a new HeaderEntryInfo struct with a given offset and size.
		fn new(offset: usize, size: u32) -> Self {
			HeaderChunkInfo { offset, size }
		}
	}

	/// A struct representing the header of a wav file. It stores the offset and size of each chunk in the header,
	/// the format information and the current size of the file in bytes.
	#[derive(Debug, Clone, PartialEq, Eq)]
	pub struct WavHeader {
		header_info: HashMap<ChunkIdentifier, HeaderChunkInfo>,
		pub fmt_chunk: FmtChunk,
		current_file_size: usize, // convenience field for keeping track of the current file size
	}

	impl WavHeader {
		/// Constructs a new WavHeader struct using the provided header information.
		fn new(
			header_info: HashMap<ChunkIdentifier, HeaderChunkInfo>,
			fmt_chunk: FmtChunk,
			current_file_size: usize,
		) -> Self {
			assert!(header_info.contains_key(&DATA.into()), "Header info must contain a DATA chunk");
			println!("Creating new WavHeader with fmt chunk: {:?}", fmt_chunk);
			Self {
				header_info,
				fmt_chunk,
				current_file_size,
			}
		}

		/// Creates a new WavHeader with the given sample rate, number of channels and number of samples.
		/// This function inserts the required fields into the WavHeader such as RIFF, WAVE, FMT and DATA.
		pub fn new_header(sample_rate: i32, n_channels: u16, n_samples: usize, wav_type: WavType) -> Self {
			println!("Creating new header with sample rate: {sample_rate}, channels: {n_channels}, samples: {n_samples}");

			// Calculate sizes of data chunks
			let bits_per_sample = wav_type.n_bits();
			let data_size_bytes = n_samples * (bits_per_sample / 8) as usize;
			let fmt_data_size = match wav_type {
				WavType::Pcm16 | WavType::Pcm32 | WavType::Float32 | WavType::Float64 => FMT_SIZE_BASE_SIZE,
				WavType::EPcm16 | WavType::EPcm32 | WavType::EFloat32 | WavType::EFloat64 => FMT_SIZE_EXTENDED_SIZE,
			};

			let mut header_info: HashMap<ChunkIdentifier, HeaderChunkInfo> = HashMap::new();

			// The RIFF chunk size should be:
			// sizeof("WAVE") + sizeof(fmt_chunk) + sizeof(data_chunk)
			// where each chunk includes its header (identifier + size field)
			let riff_chunk_size = 4 + // "WAVE"
				(8 + fmt_data_size) + // "fmt " + size + data
				(8 + data_size_bytes); // "data" + size + data

			header_info.insert(
				RIFF.into(),
				HeaderChunkInfo::new(0, (riff_chunk_size - 8) as u32),
			);
			header_info.insert(
				FMT.into(),
				HeaderChunkInfo::new(12, fmt_data_size as u32)
			);
			header_info.insert(
				DATA.into(),
				HeaderChunkInfo::new(20 + fmt_data_size, data_size_bytes as u32),
			);

			// Total file size includes everything
			let current_file_size = riff_chunk_size; // RIFF + size + (chunk_size - 8)

			// Create fmt chunk
			// TODO: This should be its own function
			let (main_format, sub_format) = match wav_type {
				WavType::Pcm16 | WavType::Pcm32 => (
					FormatCode::WAV_FORMAT_PCM,
					FormatCode::WAV_FORMAT_PCM
				),
				WavType::Float32 | WavType::Float64 => (
					FormatCode::WAV_FORMAT_IEEE_FLOAT,
					FormatCode::WAV_FORMAT_IEEE_FLOAT,
				),
				WavType::EPcm16 | WavType::EPcm32 => (
					FormatCode::WAVE_FORMAT_EXTENSIBLE,
					FormatCode::WAV_FORMAT_PCM,
				),
				WavType::EFloat32 | WavType::EFloat64 => (
					FormatCode::WAVE_FORMAT_EXTENSIBLE,
					FormatCode::WAV_FORMAT_IEEE_FLOAT,
				),
			};

			// TODO: CB/Extended Option
			// let ext_fmt_chunk = ExtFmtChunkInfo::new(CbSize::Base, bits_per_sample, 0, sub_format);
			let ext_fmt_chunk = ExtFmtChunkInfo::new(CbSize::Extended, bits_per_sample, 0, sub_format);

			let fmt_chunk = FmtChunk::new(
				main_format,
				n_channels,
				sample_rate,
				bits_per_sample,
				ext_fmt_chunk,
			);

			WavHeader {
				header_info,
				fmt_chunk,
				current_file_size,
			}
		}

		/// Returns the header in bytes, assuming that the FmtChunk is in the base format.
		pub fn as_base_bytes(&self) -> [u8; HEADER_FMT_BASE_SIZE] {
			let mut bytes = [0; HEADER_FMT_BASE_SIZE];
			bytes[0..4].copy_from_slice(&RIFF);
			let size = self.current_file_size as u32;
			bytes[4..8].copy_from_slice(&size.to_ne_bytes());
			bytes[8..12].copy_from_slice(&WAVE);

			let fmt_bytes = self.fmt_chunk.as_bytes();
			debug_assert!(
				fmt_bytes.len() == FMT_SIZE_BASE_SIZE + 8,
				"Fmt bytes length is not equal to the expected length: {} vs {}.\nFull Format Chunk: {:?}\nFull Format Bytes: {:?}",
				fmt_bytes.len(),
				FMT_SIZE_BASE_SIZE + 8,
				self.fmt_chunk,
				fmt_bytes
			);
			bytes[12..12 + fmt_bytes.len()].copy_from_slice(&fmt_bytes);
			bytes
		}

		/// Returns the header in bytes, assuming that the FmtChunk is in the extensible format.
		pub fn as_extended_bytes(&self) -> [u8; HEADER_FMT_EXTENDED_SIZE] {
			let mut bytes = [0; HEADER_FMT_EXTENDED_SIZE];
			bytes[0..4].copy_from_slice(&RIFF);
			let size = self.current_file_size as u32;
			bytes[4..8].copy_from_slice(&size.to_ne_bytes());
			bytes[8..12].copy_from_slice(&WAVE);
			let fmt_bytes = self.fmt_chunk.as_bytes();
			bytes[12..12 + fmt_bytes.len()].copy_from_slice(&fmt_bytes);
			bytes
		}
	}

	/// Wrapper around a 4 byte buffer. Used for storing and displaying/debugging the chunk identifier of a chunk.
	#[derive(Copy, Clone, PartialEq, Eq, Hash)]
	struct ChunkIdentifier {
		identifier: [u8; 4],
	}

	impl ChunkIdentifier {
		fn new(identifier: [u8; 4]) -> Self {
			ChunkIdentifier { identifier }
		}
	}

	impl Into<ChunkIdentifier> for [u8; 4] {
		fn into(self) -> ChunkIdentifier {
			ChunkIdentifier::new(self)
		}
	}

	impl Debug for ChunkIdentifier {
		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
			let out_str = std::str::from_utf8(&self.identifier).unwrap_or("Invalid identifier");
			write!(f, "{}", out_str)
		}
	}

	// This shouldn't cause too many performance issues. Would wager than there is only ever the core header chunks and maybe a handful more.
	// Each iteration is simply just a read of 8 (4+4) bytes.
	fn discover_all_header_chunks(reader: &mut Box<dyn super::ReadSeek>) -> HashMap<ChunkIdentifier, HeaderChunkInfo> {
		let mut entries: HashMap<ChunkIdentifier, HeaderChunkInfo> = HashMap::new();

		// create a reusable buffer for reading header chunks
		let mut buf = [0; 4];
		// The first 4 bytes of the file should be the RIFF chunk
		reader.read_exact(&mut buf).unwrap();
		if !buf_eq(&RIFF, &buf) {
			panic!("File is not a valid RIFF file");
		}

		reader.read_exact(&mut buf).unwrap(); // read the next 4 bytes which should be the size of the file

		let file_size: u32 = buf[0] as u32 | (buf[1] as u32) << 8 | (buf[2] as u32) << 16 | (buf[3] as u32) << 24;
		entries.insert(RIFF.into(), HeaderChunkInfo::new(0, file_size as u32));

		// The next 4 bytes should be the RIFF type id
		reader.seek_relative(4).unwrap();
		// let _: ChunkIdentifier = buf.into();

		while let Ok(_) = reader.read_exact(&mut buf) {
			let chunk_identifier: ChunkIdentifier = buf.into();
			reader.read_exact(&mut buf).unwrap();
			let chunk_size: u32 = buf[0] as u32 | (buf[1] as u32) << 8 | (buf[2] as u32) << 16 | (buf[3] as u32) << 24;
			entries.insert(
				chunk_identifier,
				HeaderChunkInfo::new(reader.stream_position().unwrap() as usize - 8, chunk_size),
			);

			reader.seek(SeekFrom::Current(chunk_size as i64)).unwrap();
		}

		entries
	}

	#[inline(always)]
	fn buf_eq(buf: &[u8; 4], chunk_id: &[u8; 4]) -> bool {
		buf[0] == chunk_id[0] && buf[1] == chunk_id[1] && buf[2] == chunk_id[2] && buf[3] == chunk_id[3]
	}

	/// Reads the header of a wav file and returns a tuple containing the header information and the wav encoding.
	/// Mostly for convenience, but can also be used to inspect a wav file without reading the data.
	pub fn read(readable: &mut Box<dyn super::ReadSeek>) -> WavInfo {
		// reset the buffer reader to the start of the file
		readable.seek(SeekFrom::Start(0)).unwrap();

		let header_info = discover_all_header_chunks(readable);

		if !header_info.contains_key(&FMT.into()) {
			panic!("File does not contain a fmt chunk");
		}

		let fmt_entry = header_info.get(&FMT.into()).unwrap(); // Safe since we just checked that the key exists
		let fmt_chunk: FmtChunk = read_chunk::<FmtChunk>(readable, fmt_entry);

		let wav_type = format_info_to_wav_type((
			fmt_chunk.format,
			fmt_chunk.bits_per_sample,
			fmt_chunk.format(),
		));

		let total_size = header_info.get(&RIFF.into()).unwrap().size as usize + 8;
		let wav_header = WavHeader::new(header_info, fmt_chunk, total_size);

		WavInfo {
			wav_type,
			wav_header,
		}
	}
}

use wav_type::WavType;
use conversion::ConvertTo;

/// Trait to mark a type as one of the available for being read/written in wav files
pub trait AudioSample:
	bytemuck::Pod + Default +
	ConvertTo<i16> + ConvertTo<i32> + ConvertTo<f32> + ConvertTo<f64>
{
	const WAV_TYPE: WavType;
}
impl AudioSample for i16 { const WAV_TYPE: WavType = WavType::EPcm16; }
impl AudioSample for i32 { const WAV_TYPE: WavType = WavType::EPcm32; }
impl AudioSample for f32 { const WAV_TYPE: WavType = WavType::EFloat32; }
impl AudioSample for f64 { const WAV_TYPE: WavType = WavType::EFloat64; }

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
pub fn read<S, P: AsRef<Path>>(path: P) -> (Vec<S>, usize)
where
	i16: ConvertTo<S>,
	i32: ConvertTo<S>,
	f32: ConvertTo<S>,
	f64: ConvertTo<S>,
{
	let f = std::fs::File::open(path).unwrap();
	let mut reader: Box<dyn ReadSeek> = Box::new(std::io::BufReader::new(f));
	let wav_info = header::read(&mut reader);

	let rate = wav_info.wav_header.fmt_chunk.sample_rate as usize;
	println!("Sampling Rate: {rate}");
	let channels = wav_info.wav_header.fmt_chunk.channels as usize;
	println!("Channels: {channels}");

	let mut buf: Vec<u8> = vec![];
	let _ = reader.read_to_end(&mut buf);

	let wav_type = wav_info.wav_type;
	let samples = match wav_type {
		WavType::Pcm16 | WavType::EPcm16 => {
			let temp = convert_buffer::<i16, _>(&buf, channels);
			convert_samples(&temp)
		}
		WavType::Pcm32 | WavType::EPcm32 => {
			let temp = convert_buffer::<i32, _>(&buf, channels);
			convert_samples(&temp)
		}
		WavType::Float32 | WavType::EFloat32 => {
			let temp = convert_buffer::<f32, _>(&buf, channels);
			convert_samples(&temp)
		}
		WavType::Float64 | WavType::EFloat64 => {
			let temp = convert_buffer::<f64, _>(&buf, channels);
			convert_samples(&temp)
		}
	};

	(samples, rate)
}

pub fn write<S: AudioSample, P: AsRef<Path>>(samples: Vec<S>, rate: usize, path: P) {
	let wav_header = header::WavHeader::new_header(
		rate as i32,
		1,
		samples.len(),
		S::WAV_TYPE
	);

	let f = File::create(&path).unwrap();
	let mut buf_writer: BufWriter<File> = BufWriter::new(f);

	// TODO: Base/CB/Extended Option
	let header_bytes = wav_header.as_extended_bytes();
	// let header_bytes = wav_header.as_base_bytes();
	buf_writer.write_all(&header_bytes).unwrap();

	let sample_bytes = cast_slice::<S, u8>(&samples[..]);
	let data_size_bytes = sample_bytes.len() as u32;
	buf_writer.write_all(&chunks::DATA).unwrap();
	buf_writer.write_all(&data_size_bytes.to_ne_bytes()).unwrap(); // write the data size
	buf_writer.write_all(&sample_bytes).unwrap(); // write the data
	println!("Finished writing to file: {:?}", path.as_ref());
}

#[cfg(test)]
mod test {
    use super::*;

	#[test]
	fn saveload() {
		let (samples, rate) = read::<i16, _>("./audio/Test A.wav");
		write(samples, rate, "./audio/Test A Out I16.wav");
		let (samples, rate) = read::<i32, _>("./audio/Test A.wav");
		write(samples, rate, "./audio/Test A Out I32.wav");
		let (samples, rate) = read::<f32, _>("./audio/Test A.wav");
		write(samples, rate, "./audio/Test A Out F32.wav");
		let (samples, rate) = read::<f64, _>("./audio/Test A.wav");
		write(samples, rate, "./audio/Test A Out F64.wav");
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
		let (samples, rate) = from_scratch::read::<i16, _>("./audio/Test A.wav").unwrap();
		from_scratch::write(samples, rate, "./audio/Test A Simple Out.wav").unwrap();
	}
}