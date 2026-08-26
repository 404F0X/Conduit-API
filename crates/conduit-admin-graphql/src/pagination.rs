use async_graphql::SimpleObject;

use crate::scalars::CursorScalar;

const CURSOR_PREFIX: &str = "conduit-admin-graphql:cursor:v1:";
const OFFSET_HEX_WIDTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorDecodeError {
    MissingPrefix,
    InvalidOffset,
}

/// Relay `type PageInfo` — snapshot lines 4086-4103. The cursor fields carry
/// the `Cursor` scalar (NOT plain String), matching the ent-generated
/// contract.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct PageInfo {
    pub has_next_page: bool,
    pub has_previous_page: bool,
    pub start_cursor: Option<CursorScalar>,
    pub end_cursor: Option<CursorScalar>,
}

impl PageInfo {
    pub fn empty(has_previous_page: bool, has_next_page: bool) -> Self {
        Self {
            has_next_page,
            has_previous_page,
            start_cursor: None,
            end_cursor: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge<T> {
    pub cursor: String,
    pub node: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection<T> {
    pub edges: Vec<Edge<T>>,
    pub page_info: PageInfo,
}

pub fn encode_offset_cursor(offset: u64) -> String {
    format!("{CURSOR_PREFIX}{offset:0OFFSET_HEX_WIDTH$x}")
}

pub fn decode_offset_cursor(cursor: &str) -> Result<u64, CursorDecodeError> {
    let encoded_offset = cursor
        .strip_prefix(CURSOR_PREFIX)
        .ok_or(CursorDecodeError::MissingPrefix)?;

    if encoded_offset.len() != OFFSET_HEX_WIDTH {
        return Err(CursorDecodeError::InvalidOffset);
    }

    u64::from_str_radix(encoded_offset, 16).map_err(|_| CursorDecodeError::InvalidOffset)
}

pub fn connection_from_offset_page<T>(
    items: Vec<T>,
    start_offset: u64,
    page_size: usize,
) -> Connection<T> {
    let has_next_page = items.len() > page_size;
    let has_previous_page = start_offset > 0;
    let visible_len = items.len().min(page_size);

    let edges = items
        .into_iter()
        .take(visible_len)
        .enumerate()
        .map(|(index, node)| Edge {
            cursor: encode_offset_cursor(start_offset + index as u64),
            node,
        })
        .collect::<Vec<_>>();

    let page_info = match (edges.first(), edges.last()) {
        (Some(first), Some(last)) => PageInfo {
            has_next_page,
            has_previous_page,
            start_cursor: Some(CursorScalar(first.cursor.clone())),
            end_cursor: Some(CursorScalar(last.cursor.clone())),
        },
        _ => PageInfo::empty(has_previous_page, has_next_page),
    };

    Connection { edges, page_info }
}

// =====================================================================
// S11 — Relay connection helpers mirroring ent's `build()` semantics.
//
// The Go side (internal/ent/gql_pagination.go, every `<Type>Connection.build`
// method, e.g. lines 137-173) lowers an over-fetched node slice into a relay
// Connection using the following rules:
//   - The pager fetches `first+1` (or `last+1`) rows; `len == first+1` ⇒ more
//     pages exist ⇒ `HasNextPage = true`, and the trailing row is dropped.
//   - `last+1 == len` ⇒ `HasPreviousPage = true`, trailing row dropped.
//   - `HasNextPage` starts `true` iff a `before` cursor was supplied.
//   - `HasPreviousPage` starts `true` iff an `after` cursor was supplied.
//   - When paginating with `last`, edge order is reversed so the cursor
//     points at the "end" of the page in the user-facing direction.
//
// The existing `connection_from_offset_page` above is the simpler
// offset-window variant used by the SDL smoke probe. The helpers below are
// the production-facing, ent-faithful variants.
// =====================================================================

/// Cursor pagination inputs mirroring ent's `Paginate(after, first, before, last)`
/// arguments (snapshot: every `queryX(... after: Cursor, first: Int, before:
/// Cursor, last: Int)` connection field).
#[derive(Debug, Clone, Default)]
pub struct CursorPage {
    pub after: Option<String>,
    pub first: Option<usize>,
    pub before: Option<String>,
    pub last: Option<usize>,
}

/// Build a [`Connection`] from a sorted, over-fetched node slice and the
/// ent-style pagination inputs, mirroring Go `*Connection.build` exactly
/// (`internal/ent/gql_pagination.go` lines 137-173).
///
/// The caller is responsible for applying the cursor predicate and the
/// `first+1` / `last+1` limit before passing `nodes` in the configured sort
/// direction; this function then trims the over-fetch, sets
/// `hasNextPage`/`hasPreviousPage`, derives start/end cursors by offset from
/// `start_offset`, and reverses edges when paginating backwards with `last`.
///
/// `start_offset` is the absolute offset of `nodes[0]` in the full result
/// set; it is used only to mint stable offset cursors (matching the existing
/// `encode_offset_cursor` scheme) and has no effect on the page-info flags.
pub fn connection_from_paged<T>(
    nodes: Vec<T>,
    page: &CursorPage,
    start_offset: u64,
) -> Connection<T> {
    let mut nodes = nodes;
    let mut has_next_page = page.before.is_some();
    let mut has_previous_page = page.after.is_some();

    if let Some(first) = page.first
        && first + 1 == nodes.len()
    {
        has_next_page = true;
        nodes.truncate(first);
    } else if let Some(last) = page.last
        && last + 1 == nodes.len()
    {
        has_previous_page = true;
        nodes.truncate(last);
    }

    let paginating_last = page.last.is_some();

    // When paginating backwards, ent emits edges in reverse so the client
    // still sees the page in the natural forward direction. The cursor for
    // each edge is derived from its absolute offset regardless of direction.
    let edges: Vec<Edge<T>> = if paginating_last {
        let count = nodes.len();
        nodes
            .into_iter()
            .rev()
            .enumerate()
            .map(|(rev_index, node)| {
                // Position from the start of the page in the original order.
                let forward_index = count - 1 - rev_index;
                let absolute = start_offset.saturating_add(forward_index as u64);
                Edge {
                    cursor: encode_offset_cursor(absolute),
                    node,
                }
            })
            .collect()
    } else {
        nodes
            .into_iter()
            .enumerate()
            .map(|(index, node)| {
                let absolute = start_offset.saturating_add(index as u64);
                Edge {
                    cursor: encode_offset_cursor(absolute),
                    node,
                }
            })
            .collect()
    };

    let page_info = match (edges.first(), edges.last()) {
        (Some(first), Some(last)) => PageInfo {
            has_next_page,
            has_previous_page,
            start_cursor: Some(CursorScalar(first.cursor.clone())),
            end_cursor: Some(CursorScalar(last.cursor.clone())),
        },
        _ => PageInfo::empty(has_previous_page, has_next_page),
    };

    Connection { edges, page_info }
}

/// S11 helper: wrap an already-built edge list plus an externally computed
/// [`PageInfo`] (e.g. for resolvers that bypass the offset scheme) into a
/// [`Connection`]. This is the thin adapter the task names — "given a sorted
/// slice + cursor + first/last, compute the connection; reuse the existing
/// pagination helper; add `to_connection(items, page_info) -> Connection`".
///
/// The edges keep their position in `items`; their cursors are minted from
/// `start_offset + index`, so the caller controls cursor stability by
/// choosing `start_offset`.
pub fn to_connection<T>(items: Vec<T>, page_info: PageInfo, start_offset: u64) -> Connection<T> {
    let edges = items
        .into_iter()
        .enumerate()
        .map(|(index, node)| {
            let absolute = start_offset.saturating_add(index as u64);
            Edge {
                cursor: encode_offset_cursor(absolute),
                node,
            }
        })
        .collect::<Vec<_>>();

    Connection { edges, page_info }
}

#[cfg(test)]
mod tests {
    use super::{
        CursorDecodeError, connection_from_offset_page, decode_offset_cursor, encode_offset_cursor,
    };
    use crate::scalars::CursorScalar;

    /// Unwraps the `Cursor`-scalar page-info cursors back to text for
    /// assertions.
    fn cursor_text(cursor: &Option<CursorScalar>) -> Option<&str> {
        cursor.as_ref().map(|c| c.0.as_str())
    }

    #[test]
    fn offset_cursor_round_trips() {
        let cursor = encode_offset_cursor(42);

        assert_eq!(decode_offset_cursor(&cursor), Ok(42));
        assert_eq!(
            decode_offset_cursor("wrong-prefix:000000000000002a"),
            Err(CursorDecodeError::MissingPrefix)
        );
        assert_eq!(
            decode_offset_cursor("conduit-admin-graphql:cursor:v1:not-hex"),
            Err(CursorDecodeError::InvalidOffset)
        );
    }

    #[test]
    fn connection_page_info_tracks_overfetch_and_prior_offset() {
        let connection = connection_from_offset_page(vec!["a", "b", "c"], 10, 2);

        assert_eq!(connection.edges.len(), 2);
        assert_eq!(connection.edges[0].node, "a");
        assert_eq!(connection.edges[1].node, "b");
        assert!(connection.page_info.has_next_page);
        assert!(connection.page_info.has_previous_page);
        assert_eq!(
            cursor_text(&connection.page_info.start_cursor),
            Some(encode_offset_cursor(10).as_str())
        );
        assert_eq!(
            cursor_text(&connection.page_info.end_cursor),
            Some(encode_offset_cursor(11).as_str())
        );
    }

    #[test]
    fn connection_page_info_without_overfetch_has_no_next_page() {
        let connection = connection_from_offset_page(vec![1, 2], 0, 2);

        assert_eq!(connection.edges.len(), 2);
        assert!(!connection.page_info.has_next_page);
        assert!(!connection.page_info.has_previous_page);
    }

    #[test]
    fn empty_connection_has_no_cursors() {
        let connection = connection_from_offset_page::<u64>(Vec::new(), 0, 25);

        assert!(connection.edges.is_empty());
        assert!(!connection.page_info.has_next_page);
        assert!(!connection.page_info.has_previous_page);
        assert_eq!(connection.page_info.start_cursor, None);
        assert_eq!(connection.page_info.end_cursor, None);
    }

    // -----------------------------------------------------------------
    // S11 — ent-faithful Connection/PageInfo helpers. These mirror Go
    // `*Connection.build` (`internal/ent/gql_pagination.go` lines 137-173):
    // the +1 overfetch on `first`/`last`, the `after`/`before` page-info
    // seeds, and the edge reversal when paginating backwards with `last`.
    // -----------------------------------------------------------------

    use super::{CursorPage, connection_from_paged, to_connection};
    use crate::pagination::PageInfo;

    #[test]
    fn paged_first_overfetch_sets_has_next_page_and_trims() {
        // Mirrors Go: fetch `first+1` rows; when the overfetch lands, set
        // HasNextPage and drop the trailing row.
        let page = CursorPage {
            first: Some(2),
            ..Default::default()
        };
        let connection = connection_from_paged(vec!["a", "b", "c"], &page, 0);

        assert_eq!(connection.edges.len(), 2);
        assert_eq!(connection.edges[0].node, "a");
        assert_eq!(connection.edges[1].node, "b");
        assert!(connection.page_info.has_next_page);
        assert!(!connection.page_info.has_previous_page);
        assert_eq!(
            cursor_text(&connection.page_info.start_cursor),
            Some(encode_offset_cursor(0).as_str())
        );
        assert_eq!(
            cursor_text(&connection.page_info.end_cursor),
            Some(encode_offset_cursor(1).as_str())
        );
    }

    #[test]
    fn paged_first_without_overfetch_does_not_set_has_next_page() {
        let page = CursorPage {
            first: Some(3),
            ..Default::default()
        };
        let connection = connection_from_paged(vec!["a", "b"], &page, 5);

        assert_eq!(connection.edges.len(), 2);
        assert!(!connection.page_info.has_next_page);
        assert!(!connection.page_info.has_previous_page);
        // start_offset is applied so cursors are stable across pages.
        assert_eq!(
            cursor_text(&connection.page_info.start_cursor),
            Some(encode_offset_cursor(5).as_str())
        );
        assert_eq!(
            cursor_text(&connection.page_info.end_cursor),
            Some(encode_offset_cursor(6).as_str())
        );
    }

    #[test]
    fn paged_last_overfetch_sets_has_previous_page_and_reverses_edges() {
        // Mirrors Go `*Connection.build` (gql_pagination.go lines 137-173)
        // line-for-line: when paginating backwards with `last`, the overfetch
        // row is dropped via `nodes[:len-1]` (truncate to `last` keeps the
        // first `last` elements), then the surviving edges are reversed so
        // the client reads the page in the user-facing direction.
        //
        // Input here is exactly what ent's reverse-direction query returns
        // for `last = 2`: three rows where the trailing row is the overfetch
        // sentinel. After truncation we keep ["sentinel", "page_row"], and
        // after reversal the client sees ["page_row", "sentinel"].
        let page = CursorPage {
            last: Some(2),
            ..Default::default()
        };
        let connection = connection_from_paged(vec!["sentinel", "page_row", "overfetch"], &page, 0);

        assert_eq!(connection.edges.len(), 2);
        assert_eq!(connection.edges[0].node, "page_row");
        assert_eq!(connection.edges[1].node, "sentinel");
        assert!(!connection.page_info.has_next_page);
        assert!(connection.page_info.has_previous_page);
    }

    #[test]
    fn paged_after_seed_marks_has_previous_page() {
        // Mirrors Go: `HasPreviousPage = after != nil`.
        let page = CursorPage {
            after: Some("cursor".to_owned()),
            first: Some(10),
            ..Default::default()
        };
        let connection = connection_from_paged(vec!["a"], &page, 5);

        assert!(connection.page_info.has_previous_page);
        assert!(!connection.page_info.has_next_page);
    }

    #[test]
    fn paged_before_seed_marks_has_next_page() {
        // Mirrors Go: `HasNextPage = before != nil`.
        let page = CursorPage {
            before: Some("cursor".to_owned()),
            last: Some(10),
            ..Default::default()
        };
        let connection = connection_from_paged(vec!["a"], &page, 0);

        assert!(connection.page_info.has_next_page);
        assert!(!connection.page_info.has_previous_page);
    }

    #[test]
    fn paged_empty_slice_has_no_cursors() {
        let page = CursorPage {
            first: Some(10),
            ..Default::default()
        };
        let connection = connection_from_paged::<u64>(Vec::new(), &page, 0);

        assert!(connection.edges.is_empty());
        assert!(!connection.page_info.has_next_page);
        assert!(!connection.page_info.has_previous_page);
        assert_eq!(connection.page_info.start_cursor, None);
        assert_eq!(connection.page_info.end_cursor, None);
    }

    #[test]
    fn to_connection_preserves_items_and_mints_offset_cursors() {
        let page_info = PageInfo::empty(false, true);
        let connection = to_connection(vec!["x", "y", "z"], page_info, 10);

        assert_eq!(connection.edges.len(), 3);
        assert_eq!(connection.edges[0].node, "x");
        assert_eq!(connection.edges[0].cursor, encode_offset_cursor(10));
        assert_eq!(connection.edges[2].cursor, encode_offset_cursor(12));
        // PageInfo passed through unchanged.
        assert!(!connection.page_info.has_previous_page);
        assert!(connection.page_info.has_next_page);
    }

    #[test]
    fn to_connection_empty_items_has_no_cursors() {
        let page_info = PageInfo::empty(false, false);
        let connection = to_connection::<u64>(Vec::new(), page_info, 0);

        assert!(connection.edges.is_empty());
        assert_eq!(connection.page_info.start_cursor, None);
        assert_eq!(connection.page_info.end_cursor, None);
    }
}
