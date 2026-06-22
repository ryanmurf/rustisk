//! conf_bridge_binaural_hrir_importer - Import HRIR data for binaural ConfBridge
//!
//! Converts a Head Related Impulse Response (HRIR) database (stored as a
//! multi-channel WAV file) into a C header file containing the HRIR data
//! as float arrays. The left and right ear impulse responses are interleaved
//! in the input file.
//!
//! Since the original C version uses libsndfile, and we want to avoid heavy
//! native dependencies, this Rust version reads a simple WAV file format
//! directly (PCM float 32-bit) and generates the C header.
//!
//! Port of asterisk/utils/conf_bridge_binaural_hrir_importer.c

use clap::Parser;
use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::PathBuf;
use std::process;

/// Import HRIR WAV data and generate a C header file for ConfBridge binaural audio
#[derive(Parser, Debug)]
#[command(
    name = "conf_bridge_binaural_hrir_importer",
    about = "Convert HRIR WAV database to C header file for binaural ConfBridge"
)]
struct Args {
    /// Input HRIR WAV file (multi-channel, interleaved L/R pairs)
    wav_file: PathBuf,

    /// Start index for binaural impulse responses
    index_start: u32,

    /// End index for binaural impulse responses
    index_end: u32,

    /// Output file (default: stdout)
    #[arg(short, long)]
    output: Option<PathBuf>,
}

/// Minimal WAV file header parser
#[derive(Debug)]
struct WavInfo {
    channels: u16,
    sample_rate: u32,
    #[allow(dead_code)]
    bits_per_sample: u16,
    frames: usize, // samples per channel
}

/// Read a WAV file and return the header info and float sample data.
///
/// This is a minimal WAV parser that handles PCM 16-bit and IEEE float 32-bit
/// formats, which are the most common for HRIR databases.
fn read_wav(path: &PathBuf) -> Result<(WavInfo, Vec<f32>), String> {
    let mut file = File::open(path).map_err(|e| format!("Cannot open {}: {e}", path.display()))?;

    let mut header = [0u8; 44];
    file.read_exact(&mut header)
        .map_err(|e| format!("Cannot read WAV header: {e}"))?;

    // Validate RIFF header
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err("Not a valid WAV file".to_string());
    }

    let audio_format = u16::from_le_bytes([header[20], header[21]]);
    let channels = u16::from_le_bytes([header[22], header[23]]);
    let sample_rate = u32::from_le_bytes([header[24], header[25], header[26], header[27]]);
    let bits_per_sample = u16::from_le_bytes([header[34], header[35]]);

    // Find the data chunk
    // The standard header is 44 bytes, but there may be extra chunks
    let data_size = u32::from_le_bytes([header[40], header[41], header[42], header[43]]);

    let total_samples = if bits_per_sample == 32 {
        data_size as usize / 4
    } else if bits_per_sample == 16 {
        data_size as usize / 2
    } else {
        return Err(format!("Unsupported bits per sample: {bits_per_sample}"));
    };

    let frames = if channels > 0 {
        total_samples / channels as usize
    } else {
        return Err("Zero channels in WAV file".to_string());
    };

    // Read sample data
    let mut data_bytes = vec![0u8; data_size as usize];
    file.read_exact(&mut data_bytes)
        .map_err(|e| format!("Cannot read WAV data: {e}"))?;

    let float_data = if audio_format == 3 && bits_per_sample == 32 {
        // IEEE float 32-bit
        data_bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect()
    } else if audio_format == 1 && bits_per_sample == 16 {
        // PCM 16-bit - convert to float
        data_bytes
            .chunks_exact(2)
            .map(|chunk| {
                let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
                sample as f32 / 32768.0
            })
            .collect()
    } else {
        return Err(format!(
            "Unsupported WAV format: audio_format={audio_format}, bits={bits_per_sample}"
        ));
    };

    Ok((
        WavInfo {
            channels,
            sample_rate,
            bits_per_sample,
            frames,
        },
        float_data,
    ))
}

/// Write the C header file with HRIR data.
fn write_header(
    writer: &mut dyn Write,
    filename: &str,
    info: &WavInfo,
    data: &[f32],
    index_start: u32,
    index_end: u32,
) -> io::Result<()> {
    let ir_start = (2 * index_start) as usize;
    let ir_end = ((index_end + 1) * 2) as usize;
    let impulse_size = index_end - index_start + 1;

    writeln!(writer, "/*")?;
    writeln!(writer, " * HRIR data auto-generated from: {filename}")?;
    writeln!(
        writer,
        " * Binaural index range: {} to {}",
        index_start, index_end
    )?;
    writeln!(writer, " */")?;
    writeln!(writer)?;
    writeln!(writer, "#define HRIRS_IMPULSE_LEN {}", info.frames)?;
    writeln!(writer, "#define HRIRS_IMPULSE_SIZE {impulse_size}")?;
    writeln!(
        writer,
        "#define HRIRS_SAMPLE_RATE {}",
        info.sample_rate
    )?;
    writeln!(writer)?;

    // Left ear data (even-indexed channels)
    writeln!(
        writer,
        "float hrirs_left[HRIRS_IMPULSE_SIZE][HRIRS_IMPULSE_LEN] = {{"
    )?;
    let mut ir_current = ir_start;
    while ir_current < ir_end {
        write!(writer, "{{")?;
        for j in 0..info.frames {
            let idx = ir_current * info.frames + j;
            if j + 1 < info.frames {
                write!(writer, "{:.16},", data.get(idx).copied().unwrap_or(0.0))?;
                if (j + 1) % 4 == 0 {
                    writeln!(writer)?;
                } else {
                    write!(writer, " ")?;
                }
            } else {
                write!(writer, "{:.16}", data.get(idx).copied().unwrap_or(0.0))?;
            }
        }
        ir_current += 2;
        if ir_current < ir_end {
            writeln!(writer, "}},")?;
        } else {
            writeln!(writer, "}}}};")? ;
        }
    }

    // Right ear data (odd-indexed channels)
    writeln!(writer)?;
    writeln!(
        writer,
        "float hrirs_right[HRIRS_IMPULSE_SIZE][HRIRS_IMPULSE_LEN] = {{"
    )?;
    ir_current = ir_start + 1;
    while ir_current < ir_end + 1 {
        write!(writer, "{{")?;
        for j in 0..info.frames {
            let idx = ir_current * info.frames + j;
            if j + 1 < info.frames {
                write!(writer, "{:.16},", data.get(idx).copied().unwrap_or(0.0))?;
                if (j + 1) % 4 == 0 {
                    writeln!(writer)?;
                } else {
                    write!(writer, " ")?;
                }
            } else {
                write!(writer, "{:.16}", data.get(idx).copied().unwrap_or(0.0))?;
            }
        }
        ir_current += 2;
        if ir_current < ir_end + 1 {
            writeln!(writer, "}},")?;
        } else {
            writeln!(writer, "}}}};")? ;
        }
    }

    Ok(())
}

fn main() {
    let args = Args::parse();

    if args.index_start >= args.index_end {
        eprintln!(
            "ERROR: INDEX_START ({}) must be smaller than INDEX_END ({}).",
            args.index_start, args.index_end
        );
        process::exit(1);
    }

    // Read WAV file
    let (info, data) = match read_wav(&args.wav_file) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("ERROR: {e}");
            process::exit(1);
        }
    };

    eprintln!(
        "INFO: Opened HRIR database ({}) with: channels: {}; samplerate: {}; frames: {}",
        args.wav_file.display(),
        info.channels,
        info.sample_rate,
        info.frames
    );

    // Validate indices
    if args.index_end * 2 >= info.channels as u32 {
        eprintln!(
            "ERROR: INDEX_END ({}) is out of range for HRIR database (channels: {}).",
            args.index_end, info.channels
        );
        process::exit(1);
    }

    // Write output
    let filename = args
        .wav_file
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    if let Some(ref output_path) = args.output {
        let file = File::create(output_path).unwrap_or_else(|e| {
            eprintln!("Cannot create {}: {e}", output_path.display());
            process::exit(1);
        });
        let mut writer = BufWriter::new(file);
        if let Err(e) = write_header(&mut writer, &filename, &info, &data, args.index_start, args.index_end) {
            eprintln!("Write error: {e}");
            process::exit(1);
        }
    } else {
        let stdout = io::stdout();
        let mut writer = BufWriter::new(stdout.lock());
        if let Err(e) = write_header(&mut writer, &filename, &info, &data, args.index_start, args.index_end) {
            eprintln!("Write error: {e}");
            process::exit(1);
        }
    }

    let ir_count = (args.index_end - args.index_start + 1) * 2;
    eprintln!("INFO: Successfully converted: imported {ir_count} impulse responses.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_write_header_basic() {
        let info = WavInfo {
            channels: 4,
            sample_rate: 44100,
            bits_per_sample: 32,
            frames: 2,
        };
        let data = vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8];

        let mut output = Vec::new();
        write_header(
            &mut Cursor::new(&mut output),
            "test.wav",
            &info,
            &data,
            0,
            1,
        )
        .unwrap();

        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("HRIRS_IMPULSE_LEN 2"));
        assert!(text.contains("HRIRS_IMPULSE_SIZE 2"));
        assert!(text.contains("HRIRS_SAMPLE_RATE 44100"));
        assert!(text.contains("hrirs_left"));
        assert!(text.contains("hrirs_right"));
    }

}
