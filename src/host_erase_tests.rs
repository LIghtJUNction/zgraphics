use super::*;
use crate::tests::{command, context};

#[test]
fn host_erase_retains_query_history_and_the_fixed_delete_effects() {
    for input in [
        b"".as_slice(),
        b"Ga=q,i=31,f=32,s=1,v=1;AAAA/w==",
        b"Ga=q,i=31,f=32,s=1,v=1,m=1;",
        b"Ga=q,i=31,f=32,s=1,v=1,m=1;AAAA",
        b"Ga=t,i=31,f=32,s=1,v=1,m=1;",
        b"Ga=T,i=31,f=32,s=1,v=1,C=1;AAAA/w==",
        b"Ga=T,f=32,s=1,v=1,C=1;AAAA/w==",
        b"Ga=d",
    ] {
        let mut host = Graphics::default();
        let mut protocol = Graphics::default();
        if !input.is_empty() {
            assert_eq!(command(&mut host, input), command(&mut protocol, input));
        }
        let history = host.query_only;
        let ctx = Context {
            cell_width: 11,
            cell_height: 23,
            rows: 32,
            ..context()
        };
        let mut host_replies = b"already queued".to_vec();
        let mut protocol_replies = host_replies.clone();
        let host_outcome = host.erase_visible(ctx, &mut host_replies);
        let protocol_outcome = protocol.command(b"Ga=d", ctx, &mut protocol_replies);
        assert_eq!(host.query_only, history, "input={input:?}");
        assert!(!protocol.query_only);
        assert!(host.pending.is_none());
        assert_eq!(host_replies, protocol_replies);
        assert_eq!(host_outcome.cursor_move, protocol_outcome.cursor_move);
        assert_eq!(host.revision(), protocol.revision());
        assert_eq!(host.next_id, protocol.next_id);
        assert_eq!(host.image_count(), protocol.image_count());
        assert_eq!(host.placement_count(), protocol.placement_count());
        assert_eq!(host.image_bytes(), protocol.image_bytes());
        assert_eq!(
            (host.cell_width, host.cell_height, host.visible_rows),
            (11, 23, 32)
        );
        assert_eq!(host.is_query_only(), history, "input={input:?}");
    }
}

#[test]
fn host_erase_cannot_hide_anonymous_image_or_external_delete_history() {
    let mut graphics = Graphics::default();
    command(&mut graphics, b"Ga=T,f=32,s=1,v=1,C=1;AAAA/w==");
    assert_eq!((graphics.image_count(), graphics.placement_count()), (1, 1));
    let next_id = graphics.next_id;
    graphics.erase_visible(context(), &mut Vec::new());
    assert_eq!((graphics.image_count(), graphics.placement_count()), (0, 0));
    assert_eq!(graphics.next_id, next_id);
    assert!(!graphics.is_query_only());

    let mut graphics = Graphics::default();
    command(&mut graphics, b"Ga=d");
    graphics.erase_visible(context(), &mut Vec::new());
    assert_eq!((graphics.image_count(), graphics.placement_count()), (0, 0));
    assert!(!graphics.is_query_only());
}
