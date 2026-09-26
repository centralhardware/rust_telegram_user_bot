//! `peer_names`, read and written through its Buffer (`peer_names_buffer`).

use super::*;

impl ClickhouseDb {
    pub(super) async fn load_peer_names(&self, peer_id: i64) -> Option<PeerNames> {
        match self
            .ch
            .query(
                "SELECT peer_id, \
                        argMax(peer_names_buffer.title, peer_names_buffer.updated_at) AS title, \
                        argMax(peer_names_buffer.first_name, peer_names_buffer.updated_at) AS first_name, \
                        argMax(peer_names_buffer.last_name, peer_names_buffer.updated_at) AS last_name, \
                        argMax(peer_names_buffer.usernames, peer_names_buffer.updated_at) AS usernames, \
                        argMax(peer_names_buffer.community_id, peer_names_buffer.updated_at) AS community_id, \
                        max(peer_names_buffer.updated_at) AS updated_at \
                 FROM peer_names_buffer WHERE peer_id = ? \
                 GROUP BY peer_id",
            )
            .bind(peer_id)
            .fetch_one::<PeerNames>()
            .await
        {
            Ok(row) => Some(row),
            Err(clickhouse::error::Error::RowNotFound) => {
                debug!("peer {peer_id} has no stored names");
                None
            }
            Err(e) => {
                error!("looking up names for peer {peer_id}: {e}");
                None
            }
        }
    }

    pub(super) async fn write_peer_names(&self, names: &PeerNames) {
        // The Buffer table in front of `peer_names` (migration 041).
        if let Err(e) =
            insert_rows(&self.ch, "peer_names_buffer", std::slice::from_ref(names)).await
        {
            error!("insert into peer_names_buffer: {e}");
        }
    }
}
