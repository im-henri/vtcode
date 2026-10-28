//! One cached native tree transaction, rather than one network round trip per
//! expanded directory. Sorting, ignore rules and recursive name search run beside
//! the files, using the same implementation as the standalone Explorer.
use super::*;

#[cfg(test)]
#[path = "remote_explorer_tests.rs"]
mod tests;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Query {
    epoch: u64,
    expanded: Vec<RemotePath>,
    sort: crate::explorer::ExplorerSortMode,
    find: Option<Find>,
    source_control: bool,
    #[serde(default)]
    files_exclude: crate::settings::ExcludePatterns,
}
#[derive(Debug, Serialize, Deserialize)]
struct Find {
    text: String,
    filter: bool,
    fuzzy: bool,
    regex: bool,
    collapsed: Vec<RemotePath>,
}
#[derive(Debug, Serialize, Deserialize)]
struct Row {
    path: RemotePath,
    name: String,
    directory: bool,
    depth: usize,
    expanded: bool,
    status: crate::explorer::GitStatus,
    summary: crate::explorer::GitStatusSummary,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Tree {
    entries: Vec<Row>,
    count: usize,
    matches: Vec<(RemotePath, crate::explorer_find::Match)>,
    error: Option<String>,
    filtered: Option<Vec<Row>>,
}
fn encode_rows(root: &Path, entries: Vec<crate::explorer::FileEntry>) -> io::Result<Vec<Row>> {
    if entries.len() > 100_000 {
        return Err(io::Error::other("Remote Explorer entry limit exceeded"));
    }
    entries
        .into_iter()
        .map(|entry| {
            Ok(Row {
                path: git::from_native(&entry.path, root)?,
                name: entry.name,
                directory: entry.is_dir,
                depth: entry.depth,
                expanded: entry.expanded,
                status: entry.git_status,
                summary: entry.git_status_summary,
            })
        })
        .collect()
}
pub(crate) fn read(
    root: &Path,
    cache: &mut Option<(u64, crate::explorer::FileExplorer)>,
    query: Query,
    cancel: &crate::background::CancellationToken,
) -> io::Result<Tree> {
    let (epoch, explorer) = cache.get_or_insert_with(|| {
        (
            query.epoch,
            crate::explorer::FileExplorer::new_deferred(root.to_owned()),
        )
    });
    if *epoch != query.epoch {
        explorer.invalidate_directory_cache();
        explorer.invalidate_git_decorations();
        *epoch = query.epoch;
    }
    explorer.set_source_control_enabled(query.source_control);
    explorer.set_files_exclude(&query.files_exclude);
    let expanded = query
        .expanded
        .into_iter()
        .map(|path| path.resolve(root))
        .collect::<io::Result<_>>()?;
    let find = query
        .find
        .map(|find| -> io::Result<_> {
            Ok(crate::explorer_find::Query {
                text: find.text,
                filter: find.filter,
                fuzzy: find.fuzzy,
                regex: find.regex,
                collapsed: find
                    .collapsed
                    .into_iter()
                    .map(|path| path.resolve(root))
                    .collect::<io::Result<_>>()?,
            })
        })
        .transpose()?;
    let request = explorer.headless_request(expanded, query.sort, find, cancel.clone());
    let (_, entries, matches) = crate::explorer::FileExplorer::build_refresh_entries(request);
    Ok(Tree {
        entries: encode_rows(root, entries)?,
        count: matches.count,
        error: matches.error,
        matches: matches
            .rows
            .into_iter()
            .map(|(path, row)| Ok((git::from_native(&path, root)?, row)))
            .collect::<io::Result<_>>()?,
        filtered: matches
            .filtered_tree
            .map(|entries| encode_rows(root, entries))
            .transpose()?,
    })
}
impl workspace::Workspace {
    pub(crate) fn explorer_tree(
        &self,
        expanded: &rustc_hash::FxHashSet<PathBuf>,
        sort: crate::explorer::ExplorerSortMode,
        find: Option<crate::explorer_find::Query>,
        source_control: bool,
        files_exclude: &crate::settings::ExcludePatterns,
        cancel: &crate::background::CancellationToken,
    ) -> io::Result<(
        Vec<crate::explorer::FileEntry>,
        crate::explorer_find::Matches,
    )> {
        let paths = |paths: rustc_hash::FxHashSet<PathBuf>| {
            paths
                .iter()
                .map(|path| self.remote_path(path))
                .collect::<io::Result<Vec<_>>>()
        };
        let query = Query {
            epoch: self
                .explorer_epoch
                .load(std::sync::atomic::Ordering::Relaxed),
            expanded: paths(expanded.clone())?,
            sort,
            source_control,
            files_exclude: files_exclude.clone(),
            find: find
                .map(|find| -> io::Result<_> {
                    Ok(Find {
                        text: find.text,
                        filter: find.filter,
                        fuzzy: find.fuzzy,
                        regex: find.regex,
                        collapsed: paths(find.collapsed)?,
                    })
                })
                .transpose()?,
        };
        let Reply::Explorer(tree) = self
            .client
            .call_cancellable(Command::Explorer(query), cancel)?
        else {
            return Err(io::Error::other("Invalid Explorer reply"));
        };
        let decode = |rows: Vec<Row>| {
            rows.into_iter()
                .map(|row| crate::explorer::FileEntry {
                    path: self.view_key(&row.path),
                    name: row.name,
                    is_dir: row.directory,
                    depth: row.depth,
                    expanded: row.expanded,
                    git_status: row.status,
                    git_status_summary: row.summary,
                })
                .collect()
        };
        Ok((
            decode(tree.entries),
            crate::explorer_find::Matches {
                count: tree.count,
                error: tree.error,
                rows: tree
                    .matches
                    .into_iter()
                    .map(|(path, row)| (self.view_key(&path), row))
                    .collect(),
                filtered_tree: tree.filtered.map(decode),
            },
        ))
    }
}
