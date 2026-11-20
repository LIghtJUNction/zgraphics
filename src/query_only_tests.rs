use super::*;
use crate::tests::{base64, command, context, send};

#[test]
fn completed_probes_do_not_acquire_protocol_identity() {
    let mut graphics = Graphics::default();
    assert!(graphics.is_query_only());
    graphics.set_viewport(11, 23, 32);
    graphics.invalidate();
    graphics.clear();
    assert!(graphics.is_query_only());
    for probe in [
        b"Ga=q,i=31,f=32,s=1,v=1;AAAA/w==".as_slice(),
        b"Ga=q,i=31,e=T,f=32,U=1,C=1",
        b"Ga=q,i=31,f=999,s=1,v=1;AAAA/w==",
        b"Ga=q,i=31,f=32,s=1,v=1;!!!!",
    ] {
        let _ = command(&mut graphics, probe);
        assert!(graphics.is_query_only());
        assert_eq!(graphics.next_id, 1);
        assert_eq!(graphics.image_count(), 0);
        assert_eq!(graphics.placement_count(), 0);
        assert_eq!(graphics.image_bytes(), 0);
    }
}

#[test]
fn a_zero_byte_query_transfer_is_pending_until_completed_or_aborted() {
    let mut graphics = Graphics::default();
    assert!(command(&mut graphics, b"Ga=q,i=31,f=32,s=1,v=1,m=1;").is_empty());
    assert_eq!(graphics.pending_bytes(), 0);
    assert!(graphics.pending.is_some());
    assert!(!graphics.is_query_only());

    // A feature probe leaves the pending transfer, including its identity,
    // untouched. Byte counts alone would incorrectly allow its loss.
    assert_eq!(
        command(&mut graphics, b"Ga=q,i=32,e=T"),
        b"\x1b_Gi=32,e=T;OK\x1b\\"
    );
    assert_eq!(graphics.pending.as_ref().unwrap().command.id, 31);
    assert!(!graphics.is_query_only());
    assert_eq!(
        command(&mut graphics, b"Gm=0;AAAA/w=="),
        b"\x1b_Gi=31;OK\x1b\\"
    );
    assert!(graphics.is_query_only());
    assert_eq!(graphics.next_id, 1);

    command(&mut graphics, b"Ga=q,i=33,f=32,s=1,v=1,m=1;");
    assert!(!graphics.is_query_only());
    graphics.abort_transfer();
    assert!(graphics.is_query_only());
}

#[test]
fn streamed_and_chunked_queries_use_the_effective_transfer_action() {
    let body = b"Ga=q,i=31,f=32,s=1,v=1;AAAA/w==";
    for split in 0..=body.len() {
        let mut graphics = Graphics::default();
        let mut stream = CommandStream::new();
        let mut replies = Vec::new();
        stream.push(&mut graphics, &body[..split], context(), &mut replies);
        stream.push(&mut graphics, &body[split..], context(), &mut replies);
        stream.finish(&mut graphics, context(), &mut replies);
        assert_eq!(replies, b"\x1b_Gi=31;OK\x1b\\", "split={split}");
        assert!(graphics.is_query_only(), "split={split}");
        assert_eq!(graphics.next_id, 1);
    }

    let mut graphics = Graphics::default();
    let mut stream = CommandStream::new();
    let mut replies = Vec::new();
    stream.push(
        &mut graphics,
        b"Ga=q,i=31,f=32,s=1,v=1,m=1;",
        context(),
        &mut replies,
    );
    stream.finish(&mut graphics, context(), &mut replies);
    assert_eq!(graphics.pending_bytes(), 0);
    assert!(!graphics.is_query_only());
    stream.push(&mut graphics, b"Gm=0;AAAA/w==", context(), &mut replies);
    stream.finish(&mut graphics, context(), &mut replies);
    assert_eq!(replies, b"\x1b_Gi=31;OK\x1b\\");
    assert!(graphics.is_query_only());

    // More than 4096 encoded bytes exercises the adapter's synthetic default
    // action continuations. They must inherit `q`, not appear to be `t`.
    let pixels = alloc::vec![0xff; 8192];
    let mut body = b"Ga=q,i=32,f=32,s=2048,v=1;".to_vec();
    body.extend(base64(&pixels));
    replies.clear();
    for chunk in body.chunks(1001) {
        stream.push(&mut graphics, chunk, context(), &mut replies);
    }
    stream.finish(&mut graphics, context(), &mut replies);
    assert_eq!(replies, b"\x1b_Gi=32;OK\x1b\\");
    assert!(graphics.is_query_only());
    assert_eq!(graphics.next_id, 1);
}

#[test]
fn stateful_commands_cannot_be_hidden_by_clear_or_later_probes() {
    for body in [
        b"Ga=t,i=31,f=32,s=1,v=1;AAAA/w==".as_slice(),
        b"Ga=T,f=32,s=1,v=1;AAAA/w==",
        b"Ga=t,i=31,f=32,s=1,v=1,m=1;",
        b"Ga=p,i=31",
        b"Ga=d,d=A",
        b"Ga=f,i=31,f=32,s=1,v=1;AAAA/w==",
        b"Ga=a,i=31,s=1",
        b"Ga=c,i=31",
    ] {
        let mut graphics = Graphics::default();
        command(&mut graphics, body);
        assert!(!graphics.is_query_only(), "body={body:?}");
        graphics.clear();
        assert_eq!(graphics.image_count(), 0);
        assert_eq!(graphics.placement_count(), 0);
        assert_eq!(graphics.pending_bytes(), 0);
        command(&mut graphics, b"Ga=q,i=32,e=T");
        assert!(!graphics.is_query_only(), "body={body:?}");
    }
}

#[test]
fn queries_preserve_live_images_and_non_query_pending_transfers() {
    let mut graphics = Graphics::default();
    send(
        &mut graphics,
        b"Ga=T,i=31,s=1,v=1,C=1",
        &[1, 2, 3, 4],
        context(),
    );
    let generation = graphics.image(31).unwrap().generation;
    assert_eq!(
        command(&mut graphics, b"Ga=q,i=31,f=32,s=1,v=1;AAAA/w=="),
        b"\x1b_Gi=31;OK\x1b\\"
    );
    assert_eq!(graphics.image(31).unwrap().pixels.as_slice(), [1, 2, 3, 4]);
    assert_eq!(graphics.image(31).unwrap().generation, generation);
    assert_eq!(graphics.placement_count(), 1);
    assert!(!graphics.is_query_only());

    command(&mut graphics, b"Ga=t,i=33,f=32,s=1,v=1,m=1;");
    command(&mut graphics, b"Ga=q,i=34,e=T");
    assert_eq!(graphics.pending.as_ref().unwrap().command.id, 33);
    assert_eq!(graphics.pending_bytes(), 0);
    assert!(!graphics.is_query_only());
    command(&mut graphics, b"Gm=0;AAAA/w==");
    assert!(graphics.image(33).is_some());
    assert!(!graphics.is_query_only());
}

#[test]
fn sixel_input_is_stateful_even_if_aborted_without_pixels() {
    let mut graphics = Graphics::default();
    graphics.begin_sixel(0, false, [0, 0, 0], None);
    assert!(!graphics.is_query_only());
    graphics.abort_sixel();
    graphics.clear();
    assert_eq!(graphics.image_count(), 0);
    assert_eq!(graphics.pending_bytes(), 0);
    assert!(!graphics.is_query_only());
}
