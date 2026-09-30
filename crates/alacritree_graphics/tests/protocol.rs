//! Transmission and reply behaviour the issue asks for beyond kitty's and
//! Ghostty's own tests: chunks split across reads, decoding off the
//! terminal lock, and the exact bytes of replies.

mod common;

use std::sync::{Arc, Condvar, Mutex};

use alacritree_common::jobs::{self, Priority};
use alacritree_graphics::Viewport;
use alacritree_graphics::placeholder::Placeholders;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{Pane, TempFile, assert_rect, command};

#[test]
fn a_chunked_transmit_and_put_split_across_reads_places_at_the_final_chunk() {
    let mut pane = Pane::with(20, 10, 100, (10, 20));
    let rgb = vec![0x40; 30 * 40 * 3];
    let encoded = STANDARD.encode(&rgb);
    let (first, rest) = encoded.split_at(1200);
    let (second, third) = rest.split_at(1200);

    let mut stream = format!("\x1b_Ga=T,f=24,i=5,s=30,v=40,m=1;{first}\x1b\\");
    stream.push_str("ab");
    stream.push_str(&format!("\x1b_Gm=1;{second}\x1b\\"));
    stream.push_str("\r\nxy");
    stream.push_str(&format!("\x1b_Gm=0;{third}\x1b\\"));
    for read in stream.as_bytes().chunks(7) {
        pane.feed(read);
    }

    assert_eq!(pane.replies(), ["\x1b_Gi=5;OK\x1b\\"]);
    // Placed where "xy" left the cursor, three cells wide and two tall, and
    // the cursor moved right by the columns and down by the rows minus one.
    assert_rect(pane.quads()[0].dest, [2.0, 1.0, 5.0, 3.0]);
    assert_eq!(pane.cursor(), (5, 2));
}

#[test]
fn a_placement_at_the_bottom_scrolls_the_screen_to_fit_the_cursor() {
    let mut pane = Pane::with(10, 4, 100, (10, 20));
    pane.feed("\x1b[4;1H");
    let reply = pane.message("a=T,f=24,i=1,s=10,v=60", vec![0; 10 * 60 * 3]);
    assert_eq!(reply.as_deref(), Some("OK"));
    assert_eq!(pane.cursor(), (1, 3));
    assert_rect(pane.quads()[0].dest, [0.0, 1.0, 1.0, 4.0]);
}

/// Every interactive worker of the job pool, occupied until released, so a
/// decode queued meanwhile waits. Dropping it releases them, so a failed
/// assertion ends the test instead of hanging it.
struct HeldPool {
    gate: Arc<(Mutex<bool>, Condvar)>,
    _holders: Vec<jobs::Job<()>>,
}

impl HeldPool {
    fn hold() -> Self {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let holders = (0..jobs::pool().background_ceiling())
            .map(|_| {
                let gate = Arc::clone(&gate);
                jobs::pool().spawn(Priority::Interactive, move |_| {
                    let (open, condvar) = &*gate;
                    let _open = condvar.wait_while(open.lock().unwrap(), |open| !*open).unwrap();
                })
            })
            .collect();
        Self { gate, _holders: holders }
    }

    fn release(&self) {
        *self.gate.0.lock().unwrap() = true;
        self.gate.1.notify_all();
    }
}

impl Drop for HeldPool {
    fn drop(&mut self) {
        self.release();
    }
}

#[test]
fn a_pending_image_draws_nothing_then_draws_after_its_decode_with_no_new_input() {
    let mut pane = Pane::new(10, 5);
    let pool = HeldPool::hold();

    let reply = pane.message("a=T,f=24,i=1,s=10,v=20", vec![0x80; 10 * 20 * 3]);
    assert_eq!(reply.as_deref(), Some("OK"), "the reply does not wait for the decode");
    assert_eq!(pane.cursor(), (1, 0), "neither does the cursor");
    assert!(pane.graphics().is_decoding());
    let generation = pane.graphics().layout_generation();
    let mut frame = alacritree_graphics::frame::ImageFrame::default();
    let viewport = alacritree_graphics::Viewport { display_offset: 0, rows: 5, columns: 10 };
    pane.term.graphics_mut().build_frame(&mut frame, viewport, &Placeholders::default());
    assert!(frame.is_empty());

    pool.release();
    pane.wakes.wait_for(1);

    assert_ne!(pane.graphics().layout_generation(), generation);
    pane.term.graphics_mut().build_frame(&mut frame, viewport, &Placeholders::default());
    assert_eq!(frame.quads().len(), 1);
    assert_eq!(frame.runs()[0].pixels.rgba()[..4], [0x80, 0x80, 0x80, 0xff]);
}

/// kitty deletes a `t=t` file once it has opened it, so an image dropped
/// before its decode runs leaves nothing behind in the temporary directory.
#[test]
fn a_temporary_file_is_deleted_even_when_its_image_goes_before_the_decode() {
    let mut pane = Pane::new(10, 5);
    let file = TempFile::new("tty-graphics-protocol-dropped", &[0; 3]);
    let pool = HeldPool::hold();

    let reply = pane.send("a=t,t=t,i=40,f=24,s=1,v=1", file.path());
    assert_eq!(reply.as_deref(), Some("_Gi=40;OK\\"));
    pane.send("a=d,d=I,i=40", b"");
    pool.release();

    assert!(!file.0.exists(), "{} was left behind", file.path());
}

/// The name marks a file the client made for the transfer, but only a
/// temporary directory holds files a remote client may have alacritree
/// delete, so one elsewhere is read and kept.
#[test]
fn a_temporary_transfer_outside_the_temporary_directory_is_not_deleted() {
    let mut pane = Pane::new(10, 5);
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("tty-graphics-protocol-kept-{}", std::process::id()));
    std::fs::write(&path, [0; 3]).unwrap();

    let reply = pane.send("a=t,t=t,i=41,f=24,s=1,v=1", path.to_str().unwrap());

    let kept = path.exists();
    let _ = std::fs::remove_file(&path);
    assert_eq!(reply.as_deref(), Some("\x1b_Gi=41;OK\x1b\\"));
    assert!(kept, "{} was deleted", path.display());
}

#[test]
fn a_frame_is_rebuilt_only_when_its_layout_or_viewport_changed() {
    let mut pane = Pane::new(10, 5);
    pane.message("a=T,f=24,i=1,s=10,v=20", vec![0; 10 * 20 * 3]);
    pane.settle();
    let mut frame = alacritree_graphics::frame::ImageFrame::default();
    let viewport = alacritree_graphics::Viewport { display_offset: 0, rows: 5, columns: 10 };
    let graphics = pane.term.graphics_mut();

    assert!(graphics.update_frame(&mut frame, viewport, &Placeholders::default()));
    let generation = frame.generation();
    assert!(!graphics.update_frame(&mut frame, viewport, &Placeholders::default()));
    assert_eq!(frame.generation(), generation);
    assert_eq!(frame.quads().len(), 1);

    assert!(graphics.update_frame(
        &mut frame,
        Viewport { display_offset: 1, ..viewport },
        &Placeholders::default()
    ));
    graphics.set_cell_pixels(20, 40);
    assert!(graphics.update_frame(
        &mut frame,
        Viewport { display_offset: 1, ..viewport },
        &Placeholders::default()
    ));
}

/// A session coming on screen rebuilds the shared frame outright, and the
/// next frame must not rebuild it again.
#[test]
fn a_frame_built_outright_is_not_rebuilt_by_the_next_update() {
    let mut pane = Pane::new(10, 5);
    pane.message("a=T,f=24,i=1,s=10,v=20", vec![0; 10 * 20 * 3]);
    pane.settle();
    let mut frame = alacritree_graphics::frame::ImageFrame::default();
    let viewport = Viewport { display_offset: 0, rows: 5, columns: 10 };
    let graphics = pane.term.graphics_mut();

    graphics.build_frame(&mut frame, viewport, &Placeholders::default());

    assert!(!graphics.update_frame(&mut frame, viewport, &Placeholders::default()));
}

#[test]
fn a_steady_screen_keeps_its_layout_generation() {
    let mut pane = Pane::new(10, 5);
    pane.message("a=T,f=24,i=1,s=10,v=20", vec![0; 10 * 20 * 3]);
    pane.settle();
    let generation = pane.graphics().layout_generation();
    pane.feed("plain text\r\nwith no images\x1b[31m in it\x1b[m");
    assert_eq!(pane.graphics().layout_generation(), generation);
    pane.feed("\n\n\n\n");
    assert_ne!(pane.graphics().layout_generation(), generation, "the scroll moved the image");
}

#[test]
fn replies_carry_kittys_exact_bytes() {
    let mut pane = Pane::new(10, 5);
    let exchanges = [
        // kitten icat's detection probe.
        ("a=q,t=d,i=31,s=1,v=1,f=24", &[0u8, 0, 0][..], "\x1b_Gi=31;OK\x1b\\"),
        (
            "a=t,i=7,f=24,s=2,v=2",
            &[0; 3],
            "\x1b_Gi=7;ENODATA:Insufficient image data: 3 < 12\x1b\\",
        ),
        ("a=t,i=7,f=77,s=1,v=1", &[0; 3], "\x1b_Gi=7;EINVAL:Unknown image format: 77\x1b\\"),
        ("a=t,i=7,f=24,s=0,v=1", &[0; 3], "\x1b_Gi=7;EINVAL:Zero width/height not allowed\x1b\\"),
        (
            "a=T,i=8,I=9,f=24,s=1,v=1",
            &[0; 3],
            "\x1b_Gi=8,I=9;EINVAL:Must not specify both image id and image number\x1b\\",
        ),
        (
            "a=t,i=7,f=24,s=10001,v=1",
            &[0; 3],
            "\x1b_Gi=7;EINVAL:Image too large, width or height greater than 10000\x1b\\",
        ),
        ("a=t,i=7,f=24,s=1,v=1,m=1", &[0; 20], "\x1b_Gi=7;EFBIG:Too much data\x1b\\"),
        (
            "a=p,i=404,p=3",
            &[],
            "\x1b_Gi=404,p=3;ENOENT:Put command refers to non-existent image with id: 404 and \
             number: 0\x1b\\",
        ),
    ];
    for (control, payload, reply) in exchanges {
        assert_eq!(pane.send(control, payload).as_deref(), Some(reply), "{control}");
    }
}

#[test]
fn a_failed_file_transmission_says_only_that_the_file_could_not_be_read() {
    let mut pane = Pane::new(10, 5);
    let missing = std::env::temp_dir().join("tty-graphics-protocol-missing");
    let control = "a=q,t=f,i=32,s=1,v=1,f=24";
    assert_eq!(
        pane.send(control, missing.to_str().unwrap()).as_deref(),
        Some("\x1b_Gi=32;EBADF:Failed to read image file\x1b\\")
    );

    let file = TempFile::new("tty-graphics-protocol-probe", &[0; 3]);
    assert_eq!(pane.send(control, file.path()).as_deref(), Some("\x1b_Gi=32;OK\x1b\\"));
}

/// Claude Code probes with `a=q,t=f` and falls back to sending the bytes
/// only on an error, so an `OK` for a file that was never read loses the
/// image. Without a client filesystem to find it in, a Linux path is
/// refused on Windows rather than guessed at.
#[test]
fn a_file_query_is_refused_for_every_path_that_cannot_be_read() {
    let mut pane = Pane::new(10, 5);
    let directory = std::env::temp_dir();
    let missing = directory.join("tty-graphics-protocol-never-written.png");
    let from_wsl = "/tmp/tty-graphics-protocol-from-a-wsl-pane.png";
    let refused = "\x1b_Gi=33;EBADF:Failed to read image file\x1b\\";

    for path in [missing.to_str().unwrap(), directory.to_str().unwrap(), from_wsl, "relative.png"] {
        let reply = pane.send("a=q,t=f,i=33,f=100", path);
        assert_eq!(reply.as_deref(), Some(refused), "{path}");
    }
    assert_eq!(pane.image_count(), 0, "a query stores nothing");
}

/// A client whose filesystem is a directory here, as a WSL distro's is to
/// Windows.
struct ClientRoot(std::path::PathBuf);

impl ClientRoot {
    fn new(name: &str) -> Self {
        let root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("client-root-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Self(root)
    }

    /// Write `data` at `path` as the client spells it.
    fn write(&self, path: &str, data: &[u8]) -> std::path::PathBuf {
        let local = self.0.join(path.trim_start_matches('/'));
        std::fs::create_dir_all(local.parent().unwrap()).unwrap();
        std::fs::write(&local, data).unwrap();
        local
    }

    fn pane(&self) -> Pane {
        let mut pane = Pane::new(10, 5);
        let root = self.0.clone();
        pane.term
            .graphics_mut()
            .set_client_paths(move |path| root.join(path.trim_start_matches('/')));
        pane
    }
}

impl Drop for ClientRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A client on another filesystem names files by its own paths, and its
/// `t=t` files are deleted only from its own temporary directories.
#[test]
fn a_client_on_another_filesystem_sends_files_by_its_own_paths() {
    let client = ClientRoot::new("sends");
    client.write("/home/lev/pixel.rgb", &[0x40; 3]);
    let in_tmp = client.write("/tmp/tty-graphics-protocol-a", &[0; 3]);
    let in_shm = client.write("/dev/shm/tty-graphics-protocol-b", &[0; 3]);
    let in_home = client.write("/home/lev/tty-graphics-protocol-c", &[0; 3]);
    let mut pane = client.pane();

    let exchanges = [
        ("a=t,t=f,i=50,f=24,s=1,v=1", "/home/lev/pixel.rgb"),
        ("a=t,t=t,i=51,f=24,s=1,v=1", "/tmp/tty-graphics-protocol-a"),
        ("a=t,t=t,i=52,f=24,s=1,v=1", "/dev/shm/tty-graphics-protocol-b"),
        ("a=t,t=t,i=53,f=24,s=1,v=1", "/home/lev/tty-graphics-protocol-c"),
    ];
    for (control, path) in exchanges {
        assert_eq!(pane.message(control, path).as_deref(), Some("OK"), "{path}");
    }
    pane.settle();

    assert_eq!(pane.pixels(50)[..4], [0x40, 0x40, 0x40, 0xff]);
    assert!(!in_tmp.exists(), "{} was left behind", in_tmp.display());
    assert!(!in_shm.exists(), "{} was left behind", in_shm.display());
    assert!(in_home.exists(), "{} was deleted", in_home.display());
}

/// kitty's refusals hold in the client's filesystem, judged by the path's
/// spelling, whatever files sit behind it here.
#[test]
fn a_client_path_is_held_to_kittys_rules_in_its_own_filesystem() {
    let client = ClientRoot::new("rules");
    for path in ["/proc/version", "/sys/kernel/x", "/dev/null", "/tmp/x"] {
        client.write(path, &[0; 3]);
    }
    let mut pane = client.pane();

    let refused = [
        "/proc/version",
        "/sys/kernel/x",
        "/dev/null",
        "//proc/version",
        "/./proc/version",
        "/tmp/../proc/version",
        r"/tmp\..\proc\version",
        "tmp/x",
    ];
    for path in refused {
        let reply = pane.send("a=q,t=f,i=54,f=24,s=1,v=1", path);
        let expected = "\x1b_Gi=54;EBADF:Failed to read image file\x1b\\";
        assert_eq!(reply.as_deref(), Some(expected), "{path}");
    }
    assert_eq!(pane.message("a=q,t=f,i=54,f=24,s=1,v=1", "/tmp/x").as_deref(), Some("OK"));
}

#[test]
fn quiet_two_silences_every_reply() {
    let mut pane = Pane::new(10, 5);
    assert_eq!(pane.send("a=t,i=7,f=24,s=2,v=2,q=2", [0; 3]), None);
    assert_eq!(pane.send("a=q,i=7,f=77,s=1,v=1,q=2", [0; 3]), None);
    assert_eq!(pane.send("a=p,i=404,q=2", b""), None);
    assert_eq!(pane.send("a=t,i=7,f=24,s=1,v=1,q=1", [0; 3]), None);
    assert_eq!(pane.send("a=t,f=24,s=2,v=2", [0; 3]), None, "no id, no reply");
}

#[test]
fn cell_and_text_area_sizes_answer_in_device_pixels() {
    let mut pane = Pane::with(80, 24, 100, (9, 18));
    pane.feed("\x1b[16t\x1b[14t");
    assert_eq!(pane.replies(), ["\x1b[6;18;9t", "\x1b[4;432;720t"]);
}

#[test]
fn an_apc_that_is_not_graphics_is_ignored() {
    let mut pane = Pane::new(10, 5);
    pane.feed("\x1b_Xnot graphics\x1b\\\x1b_G\x1b\\");
    assert!(pane.replies().is_empty());
    pane.feed(command("i=1,a=q,s=1,v=1,f=24", [0; 3]));
    assert_eq!(pane.replies().len(), 1);
}

#[test]
fn the_alternate_screen_has_its_own_images_and_starts_empty() {
    let mut pane = Pane::new(10, 5);
    pane.message("a=T,f=24,i=1,s=10,v=20", vec![0; 10 * 20 * 3]);
    pane.feed("\x1b[?1049h");
    assert_eq!(pane.image_count(), 0);
    pane.message("a=T,f=24,i=2,s=10,v=20", vec![0; 10 * 20 * 3]);
    assert_eq!(pane.quads().len(), 1);

    pane.feed("\x1b[?1049l");
    assert_eq!(pane.image_count(), 1);
    assert!(pane.graphics().pixels(1).is_some() || pane.graphics().is_decoding());
    pane.feed("\x1b[?1049h");
    assert_eq!(pane.image_count(), 0, "entering clears what the alternate screen held");
}

#[test]
fn erase_saved_lines_clears_every_image_as_kitty_does() {
    let mut pane = Pane::new(10, 5);
    pane.message("a=T,f=24,i=1,s=10,v=20", vec![0; 10 * 20 * 3]);
    pane.feed("\x1b[3J");
    assert_eq!(pane.image_count(), 0);
}

#[test]
fn resize_moves_images_with_the_lines_pushed_into_history() {
    let mut pane = Pane::with(10, 5, 100, (10, 20));
    pane.feed("\x1b[5;1H");
    pane.message("a=T,f=24,i=1,s=10,v=20,C=1", vec![0; 10 * 20 * 3]);
    assert_eq!(pane.quads()[0].dest[1], 4.0);

    struct Size(usize, usize);
    impl alacritty_terminal::grid::Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.1
        }

        fn screen_lines(&self) -> usize {
            self.1
        }

        fn columns(&self) -> usize {
            self.0
        }
    }
    pane.term.resize(Size(10, 3));
    assert_eq!(pane.quads()[0].dest[1], 2.0, "the cursor line kept its image");
    pane.term.resize(Size(10, 5));
    assert_eq!(pane.quads()[0].dest[1], 4.0, "lines pulled back from history bring it back");
}
