use std::io::Read;
use std::path::Path;

/// Pebble binary header (130 bytes for struct version >= 0x1000)
#[derive(Debug)]
pub struct PebbleProcessInfo {
    pub magic: [u8; 8],
    pub struct_version_major: u8,
    pub struct_version_minor: u8,
    pub sdk_version_major: u8,
    pub sdk_version_minor: u8,
    pub process_version_major: u8,
    pub process_version_minor: u8,
    pub load_size: u16,
    pub entry_point: u32,
    pub crc: u32,
    pub name: String,
    pub company: String,
    pub icon_resource_id: u32,
    pub sym_table_addr: u32,
    pub flags: u32,
    pub num_reloc_entries: u32,
    pub uuid: [u8; 16],
    pub resource_crc: u32,
    pub resource_timestamp: u32,
    pub virtual_size: u16,
}

impl PebbleProcessInfo {
    pub fn is_watchface(&self) -> bool {
        self.flags & 1 != 0
    }

    pub fn platform(&self) -> &'static str {
        match (self.flags >> 6) & 0xf {
            0 => "unknown",
            1 => "aplite",
            2 => "basalt",
            3 => "chalk",
            4 => "diorite",
            5 => "emery",
            _ => "unknown",
        }
    }

    pub fn uuid_str(&self) -> String {
        format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            self.uuid[0], self.uuid[1], self.uuid[2], self.uuid[3],
            self.uuid[4], self.uuid[5],
            self.uuid[6], self.uuid[7],
            self.uuid[8], self.uuid[9],
            self.uuid[10], self.uuid[11], self.uuid[12], self.uuid[13], self.uuid[14], self.uuid[15],
        )
    }
}

pub fn parse_header(data: &[u8]) -> Result<PebbleProcessInfo, String> {
    if data.len() < 130 {
        return Err(format!("Binary too small: {} bytes", data.len()));
    }

    let magic = &data[0..8];
    if &magic[0..6] != b"PBLAPP" {
        return Err(format!("Bad magic: {:?}", &magic[0..6]));
    }

    let name_bytes = &data[24..56];
    let name = String::from_utf8_lossy(
        &name_bytes[..name_bytes.iter().position(|&b| b == 0).unwrap_or(32)],
    )
    .to_string();

    let company_bytes = &data[56..88];
    let company = String::from_utf8_lossy(
        &company_bytes[..company_bytes.iter().position(|&b| b == 0).unwrap_or(32)],
    )
    .to_string();

    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&data[104..120]);

    Ok(PebbleProcessInfo {
        magic: magic.try_into().unwrap(),
        struct_version_major: data[8],
        struct_version_minor: data[9],
        sdk_version_major: data[10],
        sdk_version_minor: data[11],
        process_version_major: data[12],
        process_version_minor: data[13],
        load_size: u16::from_le_bytes([data[14], data[15]]),
        entry_point: u32::from_le_bytes([data[16], data[17], data[18], data[19]]),
        crc: u32::from_le_bytes([data[20], data[21], data[22], data[23]]),
        name,
        company,
        icon_resource_id: u32::from_le_bytes([data[88], data[89], data[90], data[91]]),
        sym_table_addr: u32::from_le_bytes([data[92], data[93], data[94], data[95]]),
        flags: u32::from_le_bytes([data[96], data[97], data[98], data[99]]),
        num_reloc_entries: u32::from_le_bytes([data[100], data[101], data[102], data[103]]),
        uuid,
        resource_crc: u32::from_le_bytes([data[120], data[121], data[122], data[123]]),
        resource_timestamp: u32::from_le_bytes([data[124], data[125], data[126], data[127]]),
        virtual_size: u16::from_le_bytes([data[128], data[129]]),
    })
}

/// Extract a platform binary from a .pbw ZIP file
pub fn extract_from_pbw(
    pbw_path: &Path,
    platform: &str,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let file = std::fs::File::open(pbw_path).map_err(|e| format!("Can't open pbw: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Bad zip: {e}"))?;

    let bin_path = format!("{platform}/pebble-app.bin");
    let res_path = format!("{platform}/app_resources.pbpack");

    let mut bin_data = Vec::new();
    archive
        .by_name(&bin_path)
        .map_err(|e| format!("No {bin_path}: {e}"))?
        .read_to_end(&mut bin_data)
        .map_err(|e| format!("Read error: {e}"))?;

    let mut res_data = Vec::new();
    archive
        .by_name(&res_path)
        .map_err(|e| format!("No {res_path}: {e}"))?
        .read_to_end(&mut res_data)
        .map_err(|e| format!("Read error: {e}"))?;

    Ok((bin_data, res_data))
}

/// Validate all guest-controlled ranges before allocating executable memory.
pub fn validate_binary(data: &[u8], info: &PebbleProcessInfo) -> Result<(), String> {
    let load = info.load_size as usize;
    let virtual_size = info.virtual_size as usize;
    if load < 130 || virtual_size < load || data.len() < load {
        return Err("invalid or truncated binary sizes".into());
    }
    let entry = (info.entry_point & !1) as usize;
    if entry < 130 || entry >= load { return Err("entry point outside executable image".into()); }
    let sym = info.sym_table_addr as usize;
    if sym.checked_add(4).is_none_or(|end| end > virtual_size) {
        return Err("symbol table pointer outside image".into());
    }
    let table_end = (info.num_reloc_entries as usize).checked_mul(4).and_then(|n| load.checked_add(n))
        .filter(|&end| end <= data.len()).ok_or("truncated relocation table")?;
    for bytes in data[load..table_end].chunks_exact(4) {
        let target = u32::from_le_bytes(bytes.try_into().unwrap()) as usize;
        if target.checked_add(4).is_none_or(|end| end > virtual_size) {
            return Err("relocation target outside image".into());
        }
    }
    Ok(())
}
