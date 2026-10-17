use night_snapshot::{Digest, SliceMem};

fn bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

fn accepts(input: &[u8]) -> bool {
    Digest::parse(&SliceMem(input), 0, input.len().try_into().unwrap()).is_ok()
}

#[test]
fn accepts_empty_and_source_position_digests() {
    assert!(accepts(&bytes(&[0, 0])));
    assert!(accepts(&bytes(&[0, 0, 1, 0x1000, 5, 6, 0, 12, 1, 0, 5, 6])));
}

#[test]
fn rejects_truncated_source_position_records() {
    let complete = bytes(&[0, 0, 1, 0x1000, 5, 6, 0, 12, 1, 0, 5, 6]);
    for length in 9..complete.len() {
        assert!(!accepts(&complete[..length]), "accepted {length} bytes");
    }
}

#[test]
fn rejects_bytes_after_the_source_position_section() {
    let mut input = bytes(&[0, 0, 0]);
    input.push(0xaa);
    assert!(!accepts(&input));
}

// Exercise guest-width arithmetic without requesting huge allocations on the
// native host. Before the bounds fix these inputs panic on wasm32.
#[cfg(target_pointer_width = "32")]
#[test]
fn rejects_overflowing_source_line_count() {
    assert!(!accepts(&bytes(&[0, 0, 1, 0x1000, 5, 6, 0, 12, u32::MAX])));
}

#[cfg(target_pointer_width = "32")]
#[test]
fn rejects_overflowing_scope_binding_count() {
    assert!(!accepts(&bytes(&[0, 1, 0x2000, u32::MAX])));
}

#[cfg(target_pointer_width = "32")]
#[test]
fn rejects_overflowing_gcthing_byte_count() {
    assert!(!accepts(&bytes(&[1, 0x1000, u32::MAX])));
}
