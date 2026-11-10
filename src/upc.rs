//! Offline Ubisoft installer package reconstruction.
//!
//! Ubisoft's `uplay_install.disc` indexes compressed slices in `packageXX.upc`;
//! `uplay_install.manifest` maps those slices back to installed files. This module
//! reconstructs the files directly and consumes each package volume as it goes.
//!
//! Parsing is separate from expansion so a download can expand a volume as soon as
//! every chunk that feeds it has arrived, instead of waiting for the whole installer.

use flate2::read::ZlibDecoder;
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

const MANIFEST_HEADER_SIZE: usize = 356;

#[derive(Clone, Copy)]
enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
}

#[derive(Clone, Copy)]
struct Field<'a> {
    number: u32,
    value: Value<'a>,
}

struct SliceTarget {
    file: usize,
    output_offset: u64,
    output_size: u64,
    download_size: u64,
    download_sha1: Vec<u8>,
}

struct PackageSlice {
    id: String,
    compressed_size: u64,
    offset: u64,
}

struct Package {
    path: PathBuf,
    size: u64,
    output_size: u64,
    slices: Vec<PackageSlice>,
}

/// Reconstructs a Ubisoft offline installer when the Epic build contains one.
/// Returns `Ok(false)` for ordinary Epic builds without the Ubisoft disc files.
pub fn unpack_if_present(
    installer_dir: &Path,
    output_dir: &Path,
    mut progress: impl FnMut(u64),
) -> Result<bool, String> {
    let Some(mut unpacker) = UbisoftUnpacker::prepare(installer_dir, output_dir)? else {
        return Ok(false);
    };
    progress(0);
    unpacker.extract_remaining(&mut progress)?;
    unpacker.finish()?;
    Ok(true)
}

/// A parsed Ubisoft installer whose volumes can be expanded one at a time, in any
/// order, as soon as the bytes of each volume are on disk.
pub struct UbisoftUnpacker {
    installer_dir: PathBuf,
    packages: Vec<Package>,
    package_indices: HashMap<PathBuf, usize>,
    slice_targets: HashMap<String, Vec<SliceTarget>>,
    output_files: Vec<PathBuf>,
    handles: HashMap<usize, File>,
    processed_slices: HashSet<String>,
    processed_bytes: u64,
    total_bytes: u64,
    last_percent: u64,
}

impl UbisoftUnpacker {
    /// Reads the disc index and manifest and creates the game files. Volume sizes are
    /// only checked when a volume is expanded, so this can run while they are still
    /// being downloaded.
    pub fn prepare(installer_dir: &Path, output_dir: &Path) -> Result<Option<Self>, String> {
        let manifest_path = installer_dir.join("uplay_install.manifest");
        let disc_path = installer_dir.join("uplay_install.disc");
        if !manifest_path.is_file() || !disc_path.is_file() {
            return Ok(None);
        }

        let manifest_bytes = fs::read(&manifest_path)
            .map_err(|error| format!("Could not read Ubisoft manifest: {error}"))?;
        if manifest_bytes.len() <= MANIFEST_HEADER_SIZE {
            return Err("Ubisoft install manifest is truncated.".into());
        }
        let mut decoder = ZlibDecoder::new(&manifest_bytes[MANIFEST_HEADER_SIZE..]);
        let mut manifest_body = Vec::new();
        decoder
            .read_to_end(&mut manifest_body)
            .map_err(|error| format!("Could not decompress Ubisoft manifest: {error}"))?;
        let manifest_fields = fields(&manifest_body)?;
        let compression_method = varint_field(&manifest_fields, 21).unwrap_or(1);
        if compression_method != 1 {
            return Err(format!(
                "Ubisoft slice compression method {compression_method} is not supported yet."
            ));
        }

        let disc_bytes = fs::read(&disc_path)
            .map_err(|error| format!("Could not read Ubisoft disc index: {error}"))?;
        let disc_root = fields(&disc_bytes)?;
        // Large installers split their volumes over several disc tables ("Disc 1" ..
        // "Disc N") inside one index. Every table has to be read, otherwise the slices
        // stored by the later volumes look absent from the disc index.
        let mut package_bodies = Vec::new();
        for disc_body in disc_root
            .iter()
            .filter(|field| field.number == 3)
            .filter_map(|field| match field.value {
                Value::Bytes(body) => Some(body),
                Value::Varint(_) => None,
            })
        {
            let disc_fields = fields(disc_body)?;
            package_bodies.extend(
                disc_fields
                    .into_iter()
                    .filter_map(|field| match field.value {
                        Value::Bytes(body) if field.number == 3 => Some(body),
                        _ => None,
                    }),
            );
        }
        if package_bodies.is_empty() {
            return Err("Ubisoft disc index has no package table.".into());
        }
        let mut packages = Vec::new();
        let mut package_slices = HashMap::<String, (usize, u64, u64)>::new();
        let mut total_package_bytes = 0u64;
        for package_body in package_bodies {
            let package_fields = fields(package_body)?;
            let name = string_field(&package_fields, 1)
                .ok_or("Ubisoft package table contains an unnamed volume.")?;
            let relative = Path::new(name);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
            {
                return Err(format!("Unsafe Ubisoft package path: {name}"));
            }
            let expected_size = varint_field(&package_fields, 2)
                .ok_or_else(|| format!("Ubisoft package {name} has no size."))?;
            let package_path = installer_dir.join(relative);
            let package_index = packages.len();
            let mut slices = Vec::new();
            for slice_field in package_fields.iter().filter(|field| field.number == 3) {
                let Value::Bytes(slice_body) = slice_field.value else {
                    continue;
                };
                let slice_fields = fields(slice_body)?;
                let raw_id = bytes_field(&slice_fields, 1)
                    .ok_or_else(|| format!("Ubisoft package {name} has a slice without an ID."))?;
                let id = canonical_id(raw_id)?;
                let compressed_size = varint_field(&slice_fields, 2)
                    .ok_or_else(|| format!("Ubisoft slice {id} has no compressed size."))?;
                let offset = varint_field(&slice_fields, 3)
                    .ok_or_else(|| format!("Ubisoft slice {id} has no package offset."))?;
                offset
                    .checked_add(compressed_size)
                    .filter(|end| *end <= expected_size)
                    .ok_or_else(|| format!("Ubisoft slice {id} extends past package {name}."))?;
                // Installers store some identical slices in several volumes so that each
                // disc works on its own; one copy is enough when unpacking them together.
                if let Some(existing) = package_slices.get(&id) {
                    if existing.2 != compressed_size {
                        return Err(format!(
                            "Ubisoft disc index repeats slice {id} with a different size."
                        ));
                    }
                } else {
                    package_slices.insert(id.clone(), (package_index, offset, compressed_size));
                }
                slices.push(PackageSlice {
                    id,
                    compressed_size,
                    offset,
                });
            }
            total_package_bytes = total_package_bytes.saturating_add(expected_size);
            packages.push(Package {
                path: package_path,
                size: expected_size,
                output_size: 0,
                slices,
            });
        }
        if packages.is_empty() {
            return Err("Ubisoft disc index contains no package volumes.".into());
        }

        let mut slice_targets = HashMap::<String, Vec<SliceTarget>>::new();
        let mut output_files = Vec::<PathBuf>::new();
        let mut output_index = HashMap::<PathBuf, usize>::new();
        for chunk_field in manifest_fields.iter().filter(|field| field.number == 4) {
            let Value::Bytes(chunk_body) = chunk_field.value else {
                continue;
            };
            let chunk_fields = fields(chunk_body)?;
            for file_field in chunk_fields.iter().filter(|field| field.number == 3) {
                let Value::Bytes(file_body) = file_field.value else {
                    continue;
                };
                let file_fields = fields(file_body)?;
                let name = string_field(&file_fields, 1)
                    .ok_or("Ubisoft manifest contains a file without a name.")?;
                let is_dir = varint_field(&file_fields, 3).unwrap_or(0) != 0;
                let size = varint_field(&file_fields, 2).unwrap_or(0);
                let relative = Path::new(name);
                if relative.is_absolute()
                    || relative
                        .components()
                        .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
                {
                    return Err(format!("Unsafe Ubisoft output path: {name}"));
                }
                let destination = output_dir.join(relative);
                if is_dir {
                    fs::create_dir_all(&destination).map_err(|error| error.to_string())?;
                    continue;
                }
                let ids: Vec<String> = file_fields
                    .iter()
                    .filter(|field| field.number == 4)
                    .filter_map(|field| match field.value {
                        Value::Bytes(raw_id) => canonical_id(raw_id).ok(),
                        Value::Varint(_) => None,
                    })
                    .collect();
                let metadata: Vec<_> = file_fields
                    .iter()
                    .filter(|field| field.number == 7)
                    .filter_map(|field| match field.value {
                        Value::Bytes(value) => Some(value),
                        Value::Varint(_) => None,
                    })
                    .collect();
                if ids.len() != metadata.len() {
                    return Err(format!(
                        "Ubisoft slice metadata count does not match for {name}."
                    ));
                }
                let file_index = output_files.len();
                let mut next_offset = 0u64;
                let mut available_slices = 0u64;
                for (id, metadata_body) in ids.into_iter().zip(metadata) {
                    let Some(&(_, _, package_compressed_size)) = package_slices.get(&id) else {
                        return Err(format!("Ubisoft disc index is missing slice {id}."));
                    };
                    let slice_fields = fields(metadata_body)?;
                    let output_size = varint_field(&slice_fields, 1)
                        .ok_or_else(|| format!("Ubisoft manifest has no size for slice {id}."))?;
                    let download_size = varint_field(&slice_fields, 2).ok_or_else(|| {
                        format!("Ubisoft manifest has no download size for {id}.")
                    })?;
                    let download_sha1 = bytes_field(&slice_fields, 3)
                        .ok_or_else(|| format!("Ubisoft manifest has no SHA-1 for {id}."))?
                        .to_vec();
                    if download_sha1.len() != 20 {
                        return Err(format!("Ubisoft slice {id} has an invalid SHA-1."));
                    }
                    let output_offset = varint_field(&slice_fields, 4).unwrap_or(next_offset);
                    next_offset = output_offset.saturating_add(output_size);
                    // The offline installer marks content it does not ship (normally
                    // optional DLC) with a zero-length slice: the manifest lists the file
                    // but no package volume holds its data.
                    if package_compressed_size == 0 {
                        continue;
                    }
                    if package_compressed_size != download_size {
                        return Err(format!("Ubisoft compressed size mismatch for slice {id}."));
                    }
                    if output_offset
                        .checked_add(output_size)
                        .filter(|end| *end <= size)
                        .is_none()
                    {
                        return Err(format!("Ubisoft slice {id} exceeds output file {name}."));
                    }
                    available_slices += 1;
                    slice_targets.entry(id).or_default().push(SliceTarget {
                        file: file_index,
                        output_offset,
                        output_size,
                        download_size,
                        download_sha1,
                    });
                }
                if next_offset != size {
                    return Err(format!(
                        "Ubisoft file size mismatch for {name}: manifest says {size}, slices cover {next_offset}."
                    ));
                }
                // Files whose data is entirely absent from the installer are left out
                // instead of being created as empty placeholders.
                if available_slices == 0 && size != 0 {
                    continue;
                }
                if output_index
                    .insert(destination.clone(), file_index)
                    .is_some()
                {
                    return Err(format!("Ubisoft manifest repeats output file {name}."));
                }
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                let created = File::create(&destination).map_err(|error| {
                    format!("Could not create {}: {error}", destination.display())
                })?;
                // Size each file up front so slices land at their final offsets instead
                // of extending the file out of order.
                created.set_len(size).map_err(|error| {
                    format!("Could not size {}: {error}", destination.display())
                })?;
                output_files.push(destination.clone());
            }
        }
        if slice_targets.is_empty() {
            return Err("Ubisoft manifest contains no installable file slices.".into());
        }

        // Consume volumes with the best space return first. This minimizes the peak
        // disk growth while compressed volumes are replaced by their game files.
        for (id, targets) in &slice_targets {
            let (package_index, _, _) = package_slices[id];
            let output_size = targets.iter().fold(0u64, |total, target| {
                total.saturating_add(target.output_size)
            });
            packages[package_index].output_size = packages[package_index]
                .output_size
                .saturating_add(output_size);
        }
        packages.sort_by_key(|package| package.output_size as i128 - package.size as i128);
        let package_indices = packages
            .iter()
            .enumerate()
            .map(|(index, package)| (package.path.clone(), index))
            .collect();
        Ok(Some(Self {
            installer_dir: installer_dir.to_path_buf(),
            packages,
            package_indices,
            slice_targets,
            output_files,
            handles: HashMap::new(),
            processed_slices: HashSet::new(),
            processed_bytes: 0,
            total_bytes: total_package_bytes,
            last_percent: 0,
        }))
    }

    /// Expands one completed volume into the game files and removes it. Paths that
    /// are not package volumes of this installer are ignored.
    pub fn extract_volume(
        &mut self,
        path: &Path,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(), String> {
        let Some(&index) = self.package_indices.get(path) else {
            return Ok(());
        };
        if !path.is_file() {
            return Ok(());
        }
        let Self {
            packages,
            slice_targets,
            output_files,
            handles,
            processed_slices,
            processed_bytes,
            total_bytes,
            last_percent,
            ..
        } = self;
        let package = &packages[index];
        let actual_size = fs::metadata(&package.path)
            .map_err(|error| format!("Could not inspect {}: {error}", package.path.display()))?
            .len();
        if actual_size != package.size {
            return Err(format!(
                "Ubisoft package size mismatch for {}: expected {}, found {}.",
                package.path.display(),
                package.size,
                actual_size
            ));
        }
        let mut input = File::open(&package.path)
            .map_err(|error| format!("Could not open {}: {error}", package.path.display()))?;
        for slice in &package.slices {
            let Some(targets) = slice_targets.get(&slice.id) else {
                continue;
            };
            if !processed_slices.insert(slice.id.clone()) {
                // A byte-identical copy stored by another volume; the first one sufficed.
                continue;
            }
            let first = &targets[0];
            if targets.iter().any(|target| {
                target.download_size != first.download_size
                    || target.output_size != first.output_size
                    || target.download_sha1 != first.download_sha1
            }) {
                return Err(format!(
                    "Conflicting metadata for Ubisoft slice {}.",
                    slice.id
                ));
            }
            if first.download_size != slice.compressed_size {
                return Err(format!(
                    "Ubisoft package size mismatch for slice {}.",
                    slice.id
                ));
            }
            let compressed_len = usize::try_from(slice.compressed_size)
                .map_err(|_| format!("Ubisoft slice {} is too large.", slice.id))?;
            let mut compressed = vec![0u8; compressed_len];
            input
                .seek(SeekFrom::Start(slice.offset))
                .and_then(|_| input.read_exact(&mut compressed))
                .map_err(|error| format!("Could not read Ubisoft slice {}: {error}", slice.id))?;
            if Sha1::digest(&compressed).as_slice() != first.download_sha1.as_slice() {
                return Err(format!(
                    "SHA-1 validation failed for Ubisoft slice {}.",
                    slice.id
                ));
            }
            let mut unpacked = Vec::new();
            ZlibDecoder::new(Cursor::new(compressed))
                .read_to_end(&mut unpacked)
                .map_err(|error| {
                    format!("Could not decompress Ubisoft slice {}: {error}", slice.id)
                })?;
            if unpacked.len() as u64 != first.output_size {
                return Err(format!(
                    "Decoded size mismatch for Ubisoft slice {}: expected {}, found {}.",
                    slice.id,
                    first.output_size,
                    unpacked.len()
                ));
            }
            for target in targets {
                if !handles.contains_key(&target.file) {
                    let file = OpenOptions::new()
                        .write(true)
                        .open(&output_files[target.file])
                        .map_err(|error| {
                            format!(
                                "Could not open {}: {error}",
                                output_files[target.file].display()
                            )
                        })?;
                    handles.insert(target.file, file);
                }
                let output = handles.get_mut(&target.file).ok_or_else(|| {
                    format!("Could not open {}", output_files[target.file].display())
                })?;
                output
                    .seek(SeekFrom::Start(target.output_offset))
                    .and_then(|_| output.write_all(&unpacked))
                    .map_err(|error| {
                        format!(
                            "Could not write {}: {error}",
                            output_files[target.file].display()
                        )
                    })?;
            }
        }
        *processed_bytes = processed_bytes.saturating_add(package.size);
        let percent = if *total_bytes > 0 {
            processed_bytes.saturating_mul(100) / *total_bytes
        } else {
            100
        };
        if percent > *last_percent {
            *last_percent = percent;
            progress(percent);
        }
        // The decompressed slices are now in their final game files. Removing this
        // volume makes room for subsequent output instead of keeping both copies.
        fs::remove_file(&package.path).map_err(|error| {
            format!(
                "Could not remove consumed package {}: {error}",
                package.path.display()
            )
        })?;
        Ok(())
    }

    /// Expands every volume still present, in the best order for disk space.
    pub fn extract_remaining(&mut self, progress: &mut dyn FnMut(u64)) -> Result<(), String> {
        let paths: Vec<PathBuf> = self
            .packages
            .iter()
            .map(|package| package.path.clone())
            .collect();
        for path in paths {
            self.extract_volume(&path, progress)?;
        }
        Ok(())
    }

    /// Fails if some slice was never expanded, then removes the consumed installer.
    pub fn finish(self) -> Result<(), String> {
        if self.processed_slices.len() != self.slice_targets.len() {
            let missing = self
                .slice_targets
                .keys()
                .find(|id| !self.processed_slices.contains(*id))
                .cloned()
                .unwrap_or_default();
            return Err(format!("Ubisoft package data is missing slice {missing}."));
        }
        fs::remove_dir_all(&self.installer_dir)
            .map_err(|error| format!("Could not remove consumed installer files: {error}"))?;
        Ok(())
    }
}

fn fields(data: &[u8]) -> Result<Vec<Field<'_>>, String> {
    let mut position = 0usize;
    let mut result = Vec::new();
    while position < data.len() {
        let key = read_varint(data, &mut position)?;
        let number = (key >> 3) as u32;
        let wire = (key & 7) as u8;
        if number == 0 {
            return Err("Invalid field in Ubisoft protobuf data.".into());
        }
        let value = match wire {
            0 => Value::Varint(read_varint(data, &mut position)?),
            1 => Value::Bytes(take(data, &mut position, 8)?),
            2 => {
                let len = usize::try_from(read_varint(data, &mut position)?)
                    .map_err(|_| "Ubisoft protobuf field is too large.")?;
                Value::Bytes(take(data, &mut position, len)?)
            }
            5 => Value::Bytes(take(data, &mut position, 4)?),
            _ => {
                return Err(format!(
                    "Unsupported protobuf wire type {wire} in Ubisoft data."
                ));
            }
        };
        result.push(Field { number, value });
    }
    Ok(result)
}

fn take<'a>(data: &'a [u8], position: &mut usize, len: usize) -> Result<&'a [u8], String> {
    let end = position
        .checked_add(len)
        .filter(|end| *end <= data.len())
        .ok_or("Unexpected end of Ubisoft protobuf data.")?;
    let bytes = &data[*position..end];
    *position = end;
    Ok(bytes)
}

fn read_varint(data: &[u8], position: &mut usize) -> Result<u64, String> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let byte = *data
            .get(*position)
            .ok_or("Unexpected end of Ubisoft protobuf varint.")?;
        *position += 1;
        if shift == 63 && byte > 1 {
            return Err("Ubisoft protobuf varint overflows 64 bits.".into());
        }
        value |= ((byte & 0x7f) as u64).checked_shl(shift).unwrap_or(0);
        if byte < 0x80 {
            return Ok(value);
        }
    }
    Err("Invalid Ubisoft protobuf varint.".into())
}

fn bytes_field<'a>(fields: &'a [Field<'a>], number: u32) -> Option<&'a [u8]> {
    fields.iter().find_map(|field| {
        if field.number != number {
            return None;
        }
        match field.value {
            Value::Bytes(bytes) => Some(bytes),
            Value::Varint(_) => None,
        }
    })
}

fn varint_field(fields: &[Field<'_>], number: u32) -> Option<u64> {
    fields.iter().find_map(|field| {
        if field.number != number {
            return None;
        }
        match field.value {
            Value::Varint(value) => Some(value),
            Value::Bytes(_) => None,
        }
    })
}

fn string_field<'a>(fields: &'a [Field<'a>], number: u32) -> Option<&'a str> {
    std::str::from_utf8(bytes_field(fields, number)?).ok()
}

fn canonical_id(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() == 20 {
        return Ok(bytes.iter().map(|byte| format!("{byte:02X}")).collect());
    }
    if bytes.len() == 40 && bytes.iter().all(u8::is_ascii_hexdigit) {
        return Ok(String::from_utf8_lossy(bytes).to_ascii_uppercase());
    }
    Err("Ubisoft slice ID has an unsupported encoding.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_varint(mut value: u64, out: &mut Vec<u8>) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if value == 0 {
                return;
            }
        }
    }

    fn push_varint_field(number: u32, value: u64, out: &mut Vec<u8>) {
        push_varint((number as u64) << 3, out);
        push_varint(value, out);
    }

    fn push_bytes(number: u32, bytes: &[u8], out: &mut Vec<u8>) {
        push_varint(((number as u64) << 3) | 2, out);
        push_varint(bytes.len() as u64, out);
        out.extend_from_slice(bytes);
    }

    fn decode_id(id: &str) -> Vec<u8> {
        (0..id.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&id[index..index + 2], 16).unwrap())
            .collect()
    }

    fn compress(data: &[u8]) -> Vec<u8> {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn disc_slice(id: &str, compressed_size: u64, offset: u64) -> Vec<u8> {
        let mut out = Vec::new();
        push_bytes(1, &decode_id(id), &mut out);
        push_varint_field(2, compressed_size, &mut out);
        push_varint_field(3, offset, &mut out);
        out
    }

    fn disc_package(name: &str, size: u64, slices: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        push_bytes(1, name.as_bytes(), &mut out);
        push_varint_field(2, size, &mut out);
        for slice in slices {
            push_bytes(3, slice, &mut out);
        }
        out
    }

    fn disc(name: &str, packages: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        push_varint_field(1, 1, &mut out);
        push_bytes(2, name.as_bytes(), &mut out);
        for package in packages {
            push_bytes(3, package, &mut out);
        }
        out
    }

    fn manifest_file(name: &str, slices: &[(&str, &[u8], u64)]) -> Vec<u8> {
        let mut out = Vec::new();
        push_bytes(1, name.as_bytes(), &mut out);
        let size: u64 = slices.iter().map(|(_, _, output)| output).sum();
        push_varint_field(2, size, &mut out);
        for (id, _, _) in slices {
            push_bytes(4, &decode_id(id), &mut out);
        }
        for (_, compressed, output) in slices {
            let mut metadata = Vec::new();
            push_varint_field(1, *output, &mut metadata);
            push_varint_field(2, compressed.len() as u64, &mut metadata);
            push_bytes(3, Sha1::digest(*compressed).as_slice(), &mut metadata);
            push_bytes(7, &metadata, &mut out);
        }
        out
    }

    fn write_installer(
        root: &Path,
        first_volume: &[u8],
        second_volume: &[u8],
    ) -> (PathBuf, PathBuf) {
        const FIRST: &str = "1111111111111111111111111111111111111111";
        const SECOND: &str = "2222222222222222222222222222222222222222";
        const DUPLICATE: &str = "3333333333333333333333333333333333333333";
        const ABSENT: &str = "4444444444444444444444444444444444444444";

        let first = compress(b"hello ");
        let second = compress(b"world");
        let duplicate = compress(b"dup");

        // Two disc tables, a slice stored in both volumes, and one slice the
        // installer marks as not shipped (optional DLC).
        let first_disc = disc(
            "TEST Disc 1",
            &[disc_package(
                "package01.upc",
                first_volume.len() as u64,
                &[
                    disc_slice(FIRST, first.len() as u64, 0),
                    disc_slice(DUPLICATE, duplicate.len() as u64, first.len() as u64),
                ],
            )],
        );
        let second_disc = disc(
            "TEST Disc 2",
            &[disc_package(
                "package02.upc",
                second_volume.len() as u64,
                &[
                    disc_slice(SECOND, second.len() as u64, 0),
                    disc_slice(DUPLICATE, duplicate.len() as u64, second.len() as u64),
                    disc_slice(ABSENT, 0, 0),
                ],
            )],
        );
        let mut disc_index = Vec::new();
        push_bytes(3, &first_disc, &mut disc_index);
        push_bytes(3, &second_disc, &mut disc_index);

        let files = [
            manifest_file(
                "game/hello.txt",
                &[(FIRST, &first, 6), (SECOND, &second, 5)],
            ),
            manifest_file("game/duplicate.bin", &[(DUPLICATE, &duplicate, 3)]),
            manifest_file("game/dlc.dat", &[(ABSENT, &[], 900)]),
        ];
        let mut chunk = Vec::new();
        for file in &files {
            push_bytes(3, file, &mut chunk);
        }
        let mut body = Vec::new();
        push_varint_field(21, 1, &mut body);
        push_bytes(4, &chunk, &mut body);
        let mut manifest = vec![0u8; MANIFEST_HEADER_SIZE];
        manifest.extend_from_slice(&compress(&body));

        let installer = root.join("Installer");
        let output = root.join("out");
        fs::create_dir_all(&installer).unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::write(installer.join("uplay_install.manifest"), &manifest).unwrap();
        fs::write(installer.join("uplay_install.disc"), &disc_index).unwrap();
        fs::write(installer.join("package01.upc"), first_volume).unwrap();
        fs::write(installer.join("package02.upc"), second_volume).unwrap();
        (installer, output)
    }

    fn scratch_root() -> PathBuf {
        // The Windows clock is coarse enough that two tests starting together would
        // otherwise pick the same folder and delete each other's files.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("ubisoft-upc-{}-{unique}", std::process::id()))
    }

    #[test]
    fn reads_every_disc_table_and_skips_slices_absent_from_the_installer() {
        let first = compress(b"hello ");
        let duplicate = compress(b"dup");
        let mut first_volume = first.clone();
        first_volume.extend_from_slice(&duplicate);
        let second = compress(b"world");
        let mut second_volume = second.clone();
        second_volume.extend_from_slice(&duplicate);

        let root = scratch_root();
        let (installer, output) = write_installer(&root, &first_volume, &second_volume);

        let result = unpack_if_present(&installer, &output, |_| {});

        assert_eq!(result, Ok(true));
        assert_eq!(
            fs::read(output.join("game/hello.txt")).unwrap(),
            b"hello world"
        );
        assert_eq!(fs::read(output.join("game/duplicate.bin")).unwrap(), b"dup");
        assert!(!output.join("game/dlc.dat").exists());
        assert!(!installer.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn expands_volumes_one_at_a_time_as_they_arrive() {
        let first = compress(b"hello ");
        let duplicate = compress(b"dup");
        let mut first_volume = first.clone();
        first_volume.extend_from_slice(&duplicate);
        let second = compress(b"world");
        let mut second_volume = second.clone();
        second_volume.extend_from_slice(&duplicate);

        let root = scratch_root();
        let (installer, output) = write_installer(&root, &first_volume, &second_volume);

        let mut unpacker = UbisoftUnpacker::prepare(&installer, &output)
            .unwrap()
            .unwrap();
        let mut progress = |_percent: u64| {};
        // Only the second volume is complete so far; the first is still downloading.
        fs::remove_file(installer.join("package01.upc")).unwrap();
        unpacker
            .extract_volume(&installer.join("package02.upc"), &mut progress)
            .unwrap();
        assert!(!installer.join("package02.upc").exists());

        // A volume that is not part of the disc index is ignored.
        unpacker
            .extract_volume(&installer.join("support.bin"), &mut progress)
            .unwrap();

        fs::write(installer.join("package01.upc"), &first_volume).unwrap();
        unpacker
            .extract_volume(&installer.join("package01.upc"), &mut progress)
            .unwrap();
        unpacker.finish().unwrap();
        assert_eq!(
            fs::read(output.join("game/hello.txt")).unwrap(),
            b"hello world"
        );
        assert_eq!(fs::read(output.join("game/duplicate.bin")).unwrap(), b"dup");
        assert!(!installer.exists());
        let _ = fs::remove_dir_all(&root);
    }
}
