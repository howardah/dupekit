use crate::codec::*;
use crate::*;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

pub(crate) fn groups_from_connection(
    connection: &Connection,
    scan_id: ScanId,
) -> Result<Vec<DuplicateGroup>> {
    if connection
        .query_row("SELECT 1 FROM scans WHERE id=?1", [scan_id], |_| Ok(()))
        .optional()?
        .is_none()
    {
        return Err(StorageError::ScanNotFound(scan_id));
    }
    let mut stmt = connection
        .prepare("SELECT id,file_size FROM duplicate_groups WHERE scan_id=?1 ORDER BY id")?;
    let raw = stmt
        .query_map([scan_id], |r| {
            Ok((r.get::<_, i64>(0)?, i64_to_u64(r.get(1)?)?))
        })?
        .collect::<std::result::Result<Vec<(i64, u64)>, _>>()?;
    raw.into_iter()
        .map(|(id, file_size)| group_from_connection(connection, id, file_size))
        .collect()
}

pub(crate) fn group_from_connection(
    connection: &Connection,
    id: i64,
    file_size: u64,
) -> Result<DuplicateGroup> {
    let mut stmt=connection.prepare("SELECT id,path,size,modified_at,modified_at_nanos,selected FROM duplicate_files WHERE group_id=?1 ORDER BY id")?;
    let raw = stmt
        .query_map([id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, i64>(5)? != 0,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut selected = Vec::new();
    let files = raw
        .into_iter()
        .map(
            |(file_id, path, size, modified_millis, modified_nanos, checked)| {
                if checked {
                    selected.push(DuplicateFileId(i64_to_u64(file_id)?));
                }
                Ok(DuplicateFile {
                    id: DuplicateFileId(i64_to_u64(file_id)?),
                    path: decode_path(&path)?,
                    size: i64_to_u64(size)?,
                    modified: modified_nanos
                        .map(nanos_to_time)
                        .or_else(|| modified_millis.map(millis_to_time))
                        .transpose()?,
                })
            },
        )
        .collect::<Result<Vec<_>>>()?;
    let mut group = DuplicateGroup::new(GroupId(i64_to_u64(id)?), file_size, files)
        .map_err(|_| StorageError::InvalidTime)?;
    for file_id in selected {
        group
            .set_selected(file_id, true)
            .map_err(|_| StorageError::InvalidTime)?;
    }
    Ok(group)
}

pub(crate) fn insert_scan(tx: &Transaction<'_>, scan: &NewScan) -> Result<ScanId> {
    tx.execute(
        "INSERT INTO scans(name,started_at,status,min_size,max_size,cache,settings_recorded) VALUES(?1,?2,'running',?3,?4,?5,1)",
        params![scan.name, time_to_millis(scan.started_at)?, scan.settings.min_size.map(u64_to_i64).transpose()?, scan.settings.max_size.map(u64_to_i64).transpose()?, scan.settings.cache as i64],
    )?;
    let id = tx.last_insert_rowid();
    for path in &scan.paths {
        tx.execute(
            "INSERT INTO scan_paths(scan_id,path,preferred) VALUES(?1,?2,?3)",
            params![id, encode_path(&path.path), path.preferred as i64],
        )?;
    }
    Ok(id)
}
pub(crate) fn insert_group(
    tx: &Transaction<'_>,
    scan_id: ScanId,
    group: &DuplicateGroup,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO duplicate_groups(scan_id,file_size) VALUES(?1,?2)",
        params![scan_id, u64_to_i64(group.file_size)?],
    )?;
    Ok(tx.last_insert_rowid())
}
pub(crate) fn insert_file(
    tx: &Transaction<'_>,
    group_id: i64,
    file: &DuplicateFile,
    selected: bool,
) -> Result<i64> {
    tx.execute("INSERT INTO duplicate_files(group_id,path,size,modified_at,modified_at_nanos,selected) VALUES(?1,?2,?3,?4,?5,?6)",params![group_id,encode_path(&file.path),u64_to_i64(file.size)?,file.modified.map(time_to_millis).transpose()?,file.modified.map(time_to_nanos).transpose()?,selected as i64])?;
    Ok(tx.last_insert_rowid())
}
