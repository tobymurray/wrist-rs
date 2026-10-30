use wrist_imu::record::{fill, frames, Mark, Recording, FRAME, MARK, READ};

fn frame(x: i16, y: i16, z: i16) -> [u8; FRAME] {
    let mut f = [0; FRAME];
    f[0..2].copy_from_slice(&x.to_le_bytes());
    f[2..4].copy_from_slice(&y.to_le_bytes());
    f[4..6].copy_from_slice(&z.to_le_bytes());
    f
}

#[test]
fn the_fill_level_is_fourteen_bits_of_whole_frames() {
    assert_eq!(fill([0x34, 0x12]), 0x1234);
    assert_eq!(fill([0xFF, 0xFF]), 0x3FFF);
    assert_eq!(frames(0), 0);
    assert_eq!(frames(5), 0);
    assert_eq!(frames(6), 1);
    assert_eq!(frames(2047), 341);
    assert_eq!(READ % FRAME, 0);
}

#[test]
fn samples_and_marks_read_back_in_order() {
    let mut buf = vec![0u8; 1024];
    let mut r = Recording::new(&mut buf);
    let mut drain = Vec::new();
    for i in 0..10i16 {
        drain.extend_from_slice(&frame(i, -i, 4096 + i));
    }
    assert!(r.push_frames(&drain));
    assert!(r.push_mark(12, 1));
    assert!(r.push_frames(&frame(-32768, 32767, 0)));
    assert!(r.push_mark(13, 2));
    assert_eq!((r.samples(), r.marks()), (11, 2));
    assert_eq!(r.sample(3), Some([3, -3, 4099]));
    assert_eq!(r.sample(10), Some([-32768, 32767, 0]));
    assert_eq!(r.sample(11), None);
    assert_eq!(r.mark(0), Some(Mark { at: 10, steps: 12, activity: 1 }));
    assert_eq!(r.mark(1), Some(Mark { at: 11, steps: 13, activity: 2 }));
    assert_eq!(r.mark(2), None);
    assert_eq!(r.sample_bytes(0..11).len(), 11 * FRAME);
    assert_eq!(r.sample_bytes(9..100), &[drain[54..60].to_vec(), frame(-32768, 32767, 0).to_vec()].concat()[..]);
}

#[test]
fn a_drain_that_would_not_leave_room_for_its_mark_is_refused_whole() {
    let mut buf = vec![0u8; 2 * FRAME + MARK + 3];
    let mut r = Recording::new(&mut buf);
    assert!(r.has_room(2));
    assert!(!r.has_room(3));
    assert!(!r.push_frames(&[0; 3 * FRAME]));
    assert_eq!(r.samples(), 0);
    assert!(r.push_frames(&[1; 2 * FRAME]));
    assert!(r.push_mark(5, 0));
    assert!(!r.has_room(0));
    assert!(!r.push_mark(6, 0));
    assert!(!r.push_frames(&[]));
    assert_eq!((r.samples(), r.marks()), (2, 1));
}

#[test]
fn partial_frames_are_refused() {
    let mut buf = vec![0u8; 256];
    let mut r = Recording::new(&mut buf);
    assert!(!r.push_frames(&[0; FRAME + 1]));
    assert_eq!(r.samples(), 0);
}

#[test]
fn the_update_staging_holds_over_28_minutes_at_50_hz() {
    let samples = (512 * 1024 - 600 * MARK) / FRAME;
    assert!(samples / 50 / 60 >= 28);
}
