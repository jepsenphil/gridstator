#![cfg(all(windows, feature = "gui"))]

#[test]
fn desktop_executable_does_not_allocate_a_console() {
    // Inspect the actual binary Cargo built, including the debug profile used by previews.
    let executable = std::fs::read(env!("CARGO_BIN_EXE_gridstator-desktop")).unwrap();
    assert_eq!(&executable[..2], b"MZ");
    let pe_offset = u32::from_le_bytes(executable[0x3c..0x40].try_into().unwrap()) as usize;
    assert_eq!(&executable[pe_offset..pe_offset + 4], b"PE\0\0");
    // The PE optional header follows the signature and 20-byte COFF header. Its subsystem
    // field is at offset 68 in both PE32 and PE32+.
    let subsystem_offset = pe_offset + 24 + 68;
    let subsystem = u16::from_le_bytes(
        executable[subsystem_offset..subsystem_offset + 2]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        subsystem, 2,
        "The desktop must use the Windows GUI subsystem"
    );
}
