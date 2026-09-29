//! kitty's `kitty_tests/graphics.py`, ported.  Each test names the one it
//! ports.  kitty asserts on normalized source rects and NDC destination
//! rects; these assert on the same rectangles in texels and cells.

mod common;

use common::{Pane, TempFile, assert_rect, byte_block, code_and_ids, opaque, png, zlib};

/// kitty's `put_helpers`: a 10x5 screen of 10x20 pixel cells, and images
/// numbered from 1.
struct Put {
    pane: Pane,
    next_id: u32,
}

const CW: u32 = 10;
const CH: u32 = 20;

impl Put {
    fn new() -> Self {
        Self { pane: Pane::new(10, 5), next_id: 0 }
    }

    /// kitty's `put_image`: an RGB image of `w`x`h` pixels, placed by
    /// `a=T`.  `extra` carries the put keys.
    fn image(&mut self, w: u32, h: u32, extra: &str) -> (u32, Option<String>) {
        self.next_id += 1;
        let id = self.next_id;
        let control = format!("a=T,f=24,i={id},s={w},v={h}{extra}");
        (id, self.pane.message(&control, vec![b'x'; (w * h * 3) as usize]))
    }

    fn image_with_id(&mut self, id: u32, w: u32, h: u32, extra: &str) -> Option<String> {
        self.next_id += 1;
        let control = format!("a=T,f=24,i={id},s={w},v={h}{extra}");
        self.pane.message(&control, vec![b'x'; (w * h * 3) as usize])
    }

    fn image_without_id(&mut self, w: u32, h: u32, extra: &str) -> Option<String> {
        self.next_id += 1;
        let control = format!("a=T,f=24,s={w},v={h}{extra}");
        self.pane.message(&control, vec![b'x'; (w * h * 3) as usize])
    }

    /// kitty's `put_ref`: `(code, ids)` of the reply.
    fn reference(&mut self, id: u32, extra: &str) -> (String, String) {
        let reply = self.pane.send(&format!("a=p,i={id}{extra}"), b"").expect("a reply");
        code_and_ids(&reply)
    }

    fn delete(&mut self, keys: &str) {
        assert_eq!(self.pane.send(&format!("a=d{keys}"), b""), None);
    }
}

fn ok() -> Option<String> {
    Some("OK".to_owned())
}

// test_load_images
#[test]
fn load_images() {
    let mut pane = Pane::new(5, 5);

    assert_eq!(pane.message("i=1,s=1,v=1,a=q", b"abcd"), ok());
    assert_eq!(pane.image_count(), 0);

    assert_eq!(pane.message("i=1,s=1,v=1,f=32", b"abcd"), ok());
    assert_eq!(pane.pixels(1), b"abcd");
    assert_eq!(pane.message("i=1,s=1,v=1,f=24", b"abc"), ok());
    assert_eq!(pane.pixels(1), opaque(b"abc"));

    assert_eq!(pane.message("i=1,s=2,v=2,m=1", b"abcd"), None);
    assert_eq!(pane.message("i=1,m=1", b"efgh"), None);
    assert_eq!(pane.message("i=1,m=1", b"ijkl"), None);
    assert_eq!(pane.message("i=1,m=0", b"mnop"), ok());
    assert_eq!(pane.pixels(1), b"abcdefghijklmnop");

    // A delete aborts a chunked transmission, and the retry starts afresh.
    assert_eq!(pane.message("i=1,s=2,v=2,m=1", b"abcd"), None);
    assert_eq!(pane.message("i=1,m=1", b"efgh"), None);
    pane.send("a=d", b"");
    assert_eq!(pane.message("i=1,s=2,v=2,m=1", b"abcd"), None);
    assert_eq!(pane.message("i=1,m=1", b"efgh"), None);
    assert_eq!(pane.message("i=1,m=1", b"ijkl"), None);
    assert_eq!(pane.message("i=1,m=0", b"1234"), ok());
    assert_eq!(pane.pixels(1), b"abcdefghijkl1234");

    let random = byte_block(32 * 1024);
    assert_eq!(pane.message("i=1,s=1024,v=8", &random), ok());
    assert_eq!(pane.pixels(1), random);

    let compressed = zlib(&random);
    assert_eq!(pane.message("i=1,s=1024,v=8,o=z", &compressed), ok());
    assert_eq!(pane.pixels(1), random);

    let half = compressed.len() / 2;
    assert_eq!(pane.message("i=1,s=1024,v=8,o=z,m=1", &compressed[..half]), None);
    assert_eq!(pane.message("i=1,m=0", &compressed[half..]), ok());
    assert_eq!(pane.pixels(1), random);

    // A temporary file is deleted once read, but only when its name says it
    // is one.
    for (name, deleted) in [("tty-graphics-protocol-load", true), ("graphics-load", false)] {
        let file = TempFile::new(name, &random);
        assert_eq!(pane.message("i=1,s=1024,v=8,t=f", file.path()), ok());
        assert_eq!(pane.pixels(1), random);
        assert!(file.0.exists());
        std::fs::write(&file.0, &compressed).unwrap();
        assert_eq!(pane.message("i=1,s=1024,v=8,t=t,o=z", file.path()), ok());
        assert_eq!(pane.pixels(1), random);
        assert_eq!(!file.0.exists(), deleted, "{name}");
    }
}

// test_load_images_from_file_edge_cases, without the FIFO and descriptor
// leak checks, which need POSIX.
#[test]
fn load_images_from_file_edge_cases() {
    let mut pane = Pane::new(5, 5);
    let random = byte_block(32 * 1024);
    let generic = Some("EBADF:Failed to read image file".to_owned());

    let mut window = b"xxx".to_vec();
    window.extend_from_slice(&random);
    window.extend_from_slice(b"yyyyy");
    let file = TempFile::new("tty-graphics-protocol-window", &window);
    let control = format!("i=1,s=1024,v=8,t=f,S={},O=3", random.len());
    assert_eq!(pane.message(&control, file.path()), ok());
    assert_eq!(pane.pixels(1), random);

    std::fs::write(&file.0, &random[..128]).unwrap();
    let control = format!("i=1,s=1024,v=8,t=f,S={}", random.len());
    assert_eq!(pane.message(&control, file.path()), generic);
    assert_eq!(pane.message("i=1,s=1024,v=8,t=f", file.path()), generic);
    assert_eq!(pane.message("i=1,s=1024,v=8,t=f,O=4096", file.path()), generic);

    let small = TempFile::new("tty-graphics-protocol-small", &random[..7]);
    let missing = std::env::temp_dir().join("tty-graphics-protocol-does-not-exist");
    let directory = std::env::temp_dir();
    for path in [small.path(), missing.to_str().unwrap(), directory.to_str().unwrap()] {
        assert_eq!(pane.message("i=1,s=1024,v=8,t=f", path), generic, "{path}");
    }
    assert_eq!(pane.message("i=1,s=1024,v=8,t=s", "/kitty-test-shm"), generic);
}

// test_load_png
#[test]
fn load_png() {
    let mut pane = Pane::new(5, 5);
    let (w, h) = (5, 3);
    let rgba = byte_block((w * h * 4) as usize);
    let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let gray: Vec<u8> = rgba.iter().step_by(4).copied().collect();

    let cases = [
        (png(w, h, png::ColorType::Rgba, &rgba), rgba.clone()),
        (png(w, h, png::ColorType::Rgb, &rgb), opaque(&rgb)),
        (
            png(w, h, png::ColorType::Grayscale, &gray),
            gray.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        ),
    ];
    for (data, expected) in cases {
        assert_eq!(pane.message("i=1,f=100", &data), ok());
        assert_eq!(pane.pixels(1), expected);
    }

    let palette = [255, 0, 0, 0, 255, 0];
    let indices: Vec<u8> = (0..w * h).map(|i| (i % 2) as u8).collect();
    let data = common::png_with_palette(w, h, png::ColorType::Indexed, &indices, Some(&palette));
    assert_eq!(pane.message("i=1,f=100", &data), ok());
    let expected: Vec<u8> = indices
        .iter()
        .flat_map(|&i| if i == 0 { [255, 0, 0, 255] } else { [0, 255, 0, 255] })
        .collect();
    assert_eq!(pane.pixels(1), expected);

    let reply = pane.message("i=1,f=100,S=20", [b'a'; 20]).unwrap();
    assert_eq!(reply.split(':').next(), Some("EBADPNG"));
}

// test_load_png_simple
#[test]
fn load_png_simple() {
    use base64::Engine;
    let png_data = base64::engine::general_purpose::STANDARD
        .decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+P+/HgAFhAJ/\
             wlseKgAAAABJRU5ErkJggg==",
        )
        .unwrap();
    let mut pane = Pane::new(5, 5);
    assert_eq!(pane.message("i=1,f=100", &png_data), ok());
    assert_eq!(pane.pixels(1), [0x00, 0xff, 0xff, 0x7f]);

    let reply = pane.message("i=1,f=100", [b'x'; 25]).unwrap();
    assert_eq!(reply.split(':').next(), Some("EBADPNG"));

    // kitty decodes in its parser and replies EBADPNG to a PNG whose body ends early.  Here the
    // header passes, so the reply is OK and the pool's decode fails: the image gets no pixels, and
    // a put finds no data as it does after kitty's EBADPNG.
    let full = png(3, 3, png::ColorType::Rgba, &byte_block(3 * 3 * 4));
    assert_eq!(pane.message("i=2,f=100", &full[..full.len() - 16]), ok());
    pane.settle();
    assert!(pane.graphics().pixels(2).is_none());
    assert_eq!(
        pane.message("a=p,i=2", b""),
        Some("ENOENT:Put command refers to image with id: 2 that could not load its data".into())
    );
}

// test_gr_operations_with_numbers
#[test]
fn operations_with_numbers() {
    let mut pane = Pane::new(5, 5);
    let li = |pane: &mut Pane, payload: &[u8], keys: &str| {
        pane.send(keys, payload).map(|reply| code_and_ids(&reply))
    };
    let pair = |code: &str, ids: &str| Some((code.to_owned(), ids.to_owned()));

    assert_eq!(li(&mut pane, b"abc", "s=1,v=1,f=24,I=1,i=3").unwrap().0, "EINVAL");
    assert_eq!(li(&mut pane, b"abc", "s=1,v=1,f=24,I=1"), pair("OK", "i=1,I=1"));
    assert_eq!(li(&mut pane, b"abc", "s=1,v=1,f=24,I=1"), pair("OK", "i=2,I=1"));
    assert_eq!(li(&mut pane, b"abc", "s=1,v=1,f=24,I=1"), pair("OK", "i=3,I=1"));
    assert_eq!(li(&mut pane, b"abc", "s=1,v=1,f=24,i=5"), pair("OK", "i=5"));
    assert_eq!(li(&mut pane, b"abc", "s=1,v=1,f=24,I=3"), pair("OK", "i=4,I=3"));

    assert_eq!(li(&mut pane, b"abcd", "s=2,v=2,m=1,I=93"), None);
    assert_eq!(li(&mut pane, b"efgh", "m=1"), None);
    assert_eq!(li(&mut pane, b"ijkx", "m=1"), None);
    assert_eq!(li(&mut pane, b"mnop", "m=0"), pair("OK", "i=6,I=93"));
    assert_eq!(pane.pixels(6), b"abcdefghijkxmnop");

    assert_eq!(li(&mut pane, b"", "a=p,c=2,r=2,I=93"), pair("OK", "i=6,I=93"));
    assert_eq!(li(&mut pane, b"", "a=p,c=2,r=2,I=94").unwrap().0, "ENOENT");

    let count = pane.image_count();
    for id in 1..=5 {
        pane.send(&format!("a=p,i={id}"), b"");
    }
    pane.send("a=d,d=N,I=94", b"");
    assert_eq!(pane.image_count(), count);
    pane.send("a=d,d=N,I=93", b"");
    assert_eq!(pane.image_count(), count - 1);
    pane.send("a=d,d=N,I=1", b"");
    assert_eq!(pane.image_count(), count - 2);

    // Deleting by number takes the newest image with it.
    let first = li(&mut pane, b"abc", "s=1,v=1,f=24,I=1117").unwrap().1;
    li(&mut pane, b"abc", "s=1,v=1,f=24,I=1117");
    let count = pane.image_count();
    pane.send("a=d,d=N,I=1117", b"");
    assert_eq!(pane.image_count(), count - 1);
    assert_eq!(li(&mut pane, b"", "a=p,I=1117").unwrap().1, first);
}

// test_image_put
#[test]
fn image_put() {
    let mut put = Put::new();
    assert_eq!(put.image(CW, CH, "").1, ok());
    let l0 = put.pane.quads();
    assert_eq!(l0.len(), 1);
    assert_rect(l0[0].src, [0.0, 0.0, 10.0, 20.0]);
    assert_rect(l0[0].dest, [0.0, 0.0, 1.0, 1.0]);
    assert_eq!(put.pane.cursor(), (1, 0));

    let keys = ",c=10,r=1,x=2,y=1,w=3,h=5,X=3,Y=1,z=-1,p=17";
    assert_eq!(put.reference(1, keys), ("OK".into(), "i=1,p=17".into()));
    let l2 = put.pane.quads();
    assert_eq!(l2.len(), 2);
    assert_eq!(l2[1], l0[0]);
    assert_rect(l2[0].src, [2.0, 1.0, 5.0, 6.0]);
    assert_rect(l2[0].dest, [1.3, 1.0 / 20.0, 11.0, 1.0]);
    assert_eq!(put.pane.cursor(), (0, 1));

    assert_eq!(put.image(10, 20, ",C=1").1, ok());
    assert_eq!(put.pane.cursor(), (0, 1));

    put.pane.feed("\x1bc");
    assert_eq!(put.image(2 * CW, 2 * CH, ",c=3").1, ok());
    assert_eq!(put.pane.cursor(), (3, 2));
    assert_rect(put.pane.quads()[0].dest, [0.0, 0.0, 3.0, 3.0]);
}

// test_graphics_put_with_pixel_offsets
#[test]
fn put_with_pixel_offsets() {
    let mut put = Put::new();
    assert_eq!(put.image(10, 20, ",X=5,Y=5").1, ok());
    assert_eq!(put.pane.cursor(), (2, 1));
}

// test_image_layer_grouping: kitty counts consecutive placements of one
// image; here those are the frame's runs.
#[test]
fn image_layer_grouping() {
    let mut put = Put::new();
    let runs = |put: &mut Put| -> Vec<u32> {
        put.pane.frame().runs().iter().map(|run| run.quads.end - run.quads.start).collect()
    };
    assert_eq!(put.image_with_id(1, 10, 20, ""), ok());
    assert_eq!(runs(&mut put), [1]);
    put.reference(1, ",c=2,r=1,p=2");
    put.reference(1, ",c=2,r=1,p=3,z=-2");
    put.reference(1, ",c=2,r=1,p=4,z=-2");
    assert_eq!(runs(&mut put), [2, 2]);
    assert_eq!(put.image_with_id(2, 8, 16, ",z=-1"), ok());
    assert_eq!(runs(&mut put), [2, 1, 2]);
}

// test_gr_scroll
#[test]
fn scroll() {
    let mut put = Put::new();
    let index = |put: &mut Put| put.pane.feed("\x1bD");
    let reverse_index = |put: &mut Put| put.pane.feed("\x1bM");

    put.image_without_id(10, 20, "");
    assert_eq!(put.pane.quads().len(), 1);
    for _ in 0..5 {
        index(&mut put);
    }
    assert_eq!(put.pane.quads().len(), 0);
    assert_eq!(put.pane.image_count(), 1);
    for _ in 0..5 - 1 {
        index(&mut put);
        assert_eq!(put.pane.quads().len(), 0);
        assert_eq!(put.pane.image_count(), 1);
    }
    index(&mut put);
    assert_eq!(put.pane.image_count(), 0);

    // Images outside the scroll region stay where they are.
    put.pane.feed("\x1bc");
    put.image(CW, CH, "");
    for _ in 0..5 - 1 {
        index(&mut put);
    }
    put.image(CW, CH, "");
    put.pane.feed("\x1b[2;4r");
    assert_eq!(put.pane.image_count(), 2);
    for _ in 0..5 + 5 {
        index(&mut put);
        assert_eq!(put.pane.image_count(), 2);
    }
    for _ in 0..5 {
        reverse_index(&mut put);
    }

    // Index clips at the top margin.
    put.image_without_id(CW, 2 * CH, ",z=-1");
    assert_eq!(put.pane.image_count(), 3);
    assert_rect(put.pane.quads()[0].src, [0.0, 0.0, 10.0, 40.0]);
    index(&mut put);
    index(&mut put);
    assert_eq!(put.pane.quads().len(), 3);
    assert_rect(put.pane.quads()[0].src, [0.0, 20.0, 10.0, 40.0]);
    index(&mut put);
    assert_eq!(put.pane.image_count(), 2);

    // Reverse index clips at the bottom margin.
    for _ in 0..5 {
        reverse_index(&mut put);
    }
    put.image_without_id(CW, 2 * CH, ",z=-1");
    assert_eq!(put.pane.image_count(), 3);
    assert_rect(put.pane.quads()[0].src, [0.0, 0.0, 10.0, 40.0]);
    while put.pane.cursor().1 != 1 {
        reverse_index(&mut put);
    }
    reverse_index(&mut put);
    reverse_index(&mut put);
    assert_rect(put.pane.quads()[0].src, [0.0, 0.0, 10.0, 20.0]);
    reverse_index(&mut put);
    assert_eq!(put.pane.image_count(), 2);

    // A scaled image is clipped at a margin, not squashed.
    put.pane.feed("\x1bc\x1b[1;3r");
    put.image_without_id(CW, 4 * CH, ",c=1,r=2");
    assert_eq!(put.pane.image_count(), 1);
    assert_rect(put.pane.quads()[0].dest, [0.0, 0.0, 1.0, 2.0]);
    while put.pane.cursor().1 != 2 {
        index(&mut put);
    }
    index(&mut put);
    let l0 = put.pane.quads();
    assert_eq!(l0.len(), 1);
    assert_rect(l0[0].src, [0.0, 40.0, 10.0, 80.0]);
    assert_rect(l0[0].dest, [0.0, 0.0, 1.0, 1.0]);
    index(&mut put);
    assert_eq!(put.pane.image_count(), 0);

    put.pane.feed("\x1bc\x1b[1;3r");
    index(&mut put);
    put.image_without_id(CW, 4 * CH, ",c=1,r=2");
    while put.pane.cursor().1 != 0 {
        reverse_index(&mut put);
    }
    reverse_index(&mut put);
    let l0 = put.pane.quads();
    assert_eq!(l0.len(), 1);
    assert_rect(l0[0].src, [0.0, 0.0, 10.0, 40.0]);
    assert_rect(l0[0].dest, [0.0, 2.0, 1.0, 3.0]);
    reverse_index(&mut put);
    assert_eq!(put.pane.image_count(), 0);

    // Scaled by columns alone, with rows from the aspect ratio.
    put.pane.feed("\x1bc\x1b[1;3r");
    put.image_without_id(2 * CW, 4 * CH, ",c=1");
    assert_rect(put.pane.quads()[0].dest, [0.0, 0.0, 1.0, 2.0]);
    while put.pane.cursor().1 != 2 {
        index(&mut put);
    }
    index(&mut put);
    let l0 = put.pane.quads();
    assert_rect(l0[0].src, [0.0, 40.0, 20.0, 80.0]);
    assert_rect(l0[0].dest, [0.0, 0.0, 1.0, 1.0]);
    index(&mut put);
    assert_eq!(put.pane.image_count(), 0);
}

// test_gr_reset
#[test]
fn reset() {
    let mut put = Put::new();
    put.image(CW, CH, "");
    assert_eq!(put.pane.quads().len(), 1);
    put.pane.feed("\x1bc");
    assert_eq!(put.pane.image_count(), 0);
    put.image(CW, CH, "");
    assert_eq!(put.pane.image_count(), 1);
    for _ in 0..5 {
        put.pane.feed("\x1bD");
    }
    put.pane.feed("\x1bc");
    assert_eq!(put.pane.image_count(), 1);
}

// test_gr_delete
#[test]
fn delete() {
    let mut put = Put::new();

    let (id, _) = put.image(CW, CH, ",a=t");
    assert_eq!(put.pane.image_count(), 1);
    put.delete(&format!(",d=I,i={id}"));
    assert_eq!(put.pane.image_count(), 0);
    let (first, _) = put.image(CW, CH, ",a=t");
    let (second, _) = put.image(CW, CH, ",a=t");
    assert_eq!(put.pane.image_count(), 2);
    put.delete(&format!(",d=R,x={first},y={second}"));
    assert_eq!(put.pane.image_count(), 0);

    put.image(CW, CH, "");
    put.delete("");
    assert_eq!(put.pane.image_count(), 1);
    assert_eq!(put.pane.quads().len(), 0);
    put.delete(",d=A");
    assert_eq!(put.pane.image_count(), 1);
    put.pane.feed("\x1bc");
    assert_eq!(put.pane.image_count(), 0);
    put.image(CW, CH, "");
    assert_eq!(put.pane.image_count(), 1);
    put.delete(",d=A");
    assert_eq!(put.pane.image_count(), 0);

    let (id, _) = put.image(CW, CH, "");
    put.delete(&format!(",d=I,i={id},p=7"));
    assert_eq!(put.pane.image_count(), 1);
    put.delete(&format!(",d=I,i={id}"));
    assert_eq!(put.pane.image_count(), 0);
    let (id, _) = put.image(CW, CH, ",p=9");
    put.delete(&format!(",d=I,i={id},p=9"));
    assert_eq!(put.pane.image_count(), 0);

    put.pane.feed("\x1bc");
    put.image(CW, CH, "");
    put.image(CW, CH, "");
    put.delete(",d=C");
    assert_eq!(put.pane.image_count(), 2);
    put.pane.feed("\x1b[1;1H");
    put.delete(",d=C");
    assert_eq!(put.pane.image_count(), 1);
    put.delete(",d=P,x=2,y=1");
    assert_eq!(put.pane.image_count(), 0);

    put.image(CW, CH, ",z=9");
    put.delete(",d=Z,z=9");
    assert_eq!(put.pane.image_count(), 0);
    put.image_with_id(1, CW, CH, "");
    put.image_with_id(2, CW, CH, "");
    put.image_with_id(3, CW, CH, "");
    put.delete(",d=R,y=2");
    assert_eq!(put.pane.image_count(), 1);
    put.delete(",d=R,x=3,y=3");
    assert_eq!(put.pane.image_count(), 0);

    // Put, delete, put.
    let id = 999_999;
    assert_eq!(put.image_with_id(id, CW, CH, ""), ok());
    assert_eq!(put.reference(id, ""), ("OK".into(), format!("i={id}")));
    put.delete(&format!(",d=i,i={id}"));
    assert_eq!(put.pane.image_count(), 1);
    assert_eq!(put.reference(id, ""), ("OK".into(), format!("i={id}")));
    put.delete(&format!(",d=I,i={id}"));
    assert_eq!(put.reference(id, ""), ("ENOENT".into(), format!("i={id}")));
    assert_eq!(put.pane.image_count(), 0);

    // Delete without freeing.
    put.pane.feed("\x1bc");
    let id = 9_999_999;
    assert_eq!(put.image_with_id(id, CW, CH, ""), ok());
    assert_eq!(put.reference(id, ""), ("OK".into(), format!("i={id}")));
    assert_eq!(put.image_with_id(id + 1, CW, CH, ""), ok());
    assert_eq!(put.reference(id + 1, ""), ("OK".into(), format!("i={}", id + 1)));
    put.delete(&format!(",d=i,i={id}"));
    assert_eq!(put.pane.image_count(), 2);
    put.delete(&format!(",d=I,i={}", id + 1));
    assert_eq!(put.pane.image_count(), 1);
}

// test_graphics_quota_enforcement, without its animation frames.  kitty
// counts an RGB image at three bytes a pixel; the quota here counts the
// decoded RGBA, four.
#[test]
fn quota_evicts_the_oldest_image() {
    let mut pane = Pane::new(5, 5);
    pane.term.graphics_mut().set_storage_limit(48 * 2);
    let li = |pane: &mut Pane, keys: &str| {
        let control = format!("s=4,v=3,f=24,{keys}");
        pane.message(&control, b"abcdefghijkl".repeat(3))
    };

    assert_eq!(li(&mut pane, "a=T,i=1"), ok());
    assert_eq!(li(&mut pane, "a=T,i=2"), ok());
    assert_eq!(pane.image_count(), 2);
    assert_eq!(li(&mut pane, "a=T,i=3"), ok());
    assert_eq!(pane.image_count(), 2);
    pane.settle();
    assert!(pane.graphics().pixels(1).is_none());
    assert!(pane.graphics().pixels(2).is_some());
    assert!(pane.graphics().pixels(3).is_some());

    pane.term.graphics_mut().set_storage_limit(47);
    assert_eq!(
        li(&mut pane, "a=T,i=4"),
        Some("ENOMEM:Image is larger than the storage quota".into())
    );
}

// test_transient_image_preferential_eviction
#[test]
fn transient_images_are_evicted_first() {
    let mut pane = Pane::new(5, 5);
    pane.term.graphics_mut().set_storage_limit(48 * 2);
    let li = |pane: &mut Pane, keys: &str| {
        let control = format!("s=4,v=3,f=24,{keys}");
        pane.message(&control, b"abcdefghijkl".repeat(3))
    };

    assert_eq!(li(&mut pane, "a=T,i=1"), ok());
    assert_eq!(li(&mut pane, "a=T,i=2,N=1"), ok());
    assert_eq!(li(&mut pane, "a=T,i=3"), ok());
    assert_eq!(pane.image_count(), 2);
    pane.settle();
    assert!(pane.graphics().pixels(2).is_none());
    assert!(pane.graphics().pixels(1).is_some());
    assert!(pane.graphics().pixels(3).is_some());
}

// test_suppressing_gr_command_responses, without its animation frames.
#[test]
fn suppressing_responses() {
    let mut pane = Pane::new(5, 5);
    assert_eq!(
        pane.message("i=1,s=10,v=10,q=1", b"abcd"),
        Some("ENODATA:Insufficient image data: 4 < 400".into())
    );
    assert_eq!(pane.message("i=1,s=10,v=10,q=2", b"abcd"), None);
    assert_eq!(pane.message("i=1,s=1,v=1,a=q,q=1", b"abcd"), None);

    assert_eq!(pane.message("i=1,s=2,v=2,m=1,q=1", b"abcd"), None);
    assert_eq!(pane.message("i=1,m=1", b"efgh"), None);
    assert_eq!(pane.message("i=1,m=1", b"ijkl"), None);
    assert_eq!(pane.message("i=1,m=0", b"mnop"), None);

    assert_eq!(pane.message("i=1,s=2,v=2,m=1,q=1", b"abcd"), None);
    assert_eq!(
        pane.message("i=1,m=0", b"mnop"),
        Some("ENODATA:Insufficient image data: 8 < 16".into())
    );
    assert_eq!(pane.message("i=1,s=2,v=2,m=1,q=2", b"abcd"), None);
    assert_eq!(pane.message("i=1,m=0", b"mnop"), None);
}
