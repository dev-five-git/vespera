//! The index of `#[cron]` functions the module walk of [`ModuleTree`] reads:
//! where each function is declared, so a stored job finds its module path.
//! A file-level declaration and one inside an inline module are indexed
//! apart, and a statically dead declaration (never compiled) is not indexed
//! at all, so neither can make a stored job look ambiguous.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use super::{CfgGate, CronJob, CronMetadata, ModuleTree, Scope};

impl ModuleTree {
    /// Module path of the cron function `fn_name` declared in `file` — in
    /// the inline module `inline` when known. Without one, the file's own
    /// declaration when it has one, else the function's declaration in an
    /// inline module.
    pub(super) fn module_of(
        &self,
        file: &str,
        inline: Option<&str>,
        fn_name: &str,
    ) -> Option<String> {
        inline
            .map_or_else(
                || {
                    let key = (file.to_owned(), fn_name.to_owned());
                    self.cron_fns
                        .get(&key)
                        .or_else(|| self.inline_cron_fns.get(&key))
                },
                |inline| {
                    self.scoped_cron_fns.get(&(
                        file.to_owned(),
                        inline.to_owned(),
                        fn_name.to_owned(),
                    ))
                },
            )
            .cloned()
    }

    /// Whether the cron function `fn_name` of `file` (in the inline module
    /// `inline` when known) is reached under two module paths. Without an
    /// inline module, only the file's own declarations count when it has one.
    pub(super) fn is_ambiguous(&self, file: &str, inline: Option<&str>, fn_name: &str) -> bool {
        inline.map_or_else(
            || {
                let key = (file.to_owned(), fn_name.to_owned());
                if self.cron_fns.contains_key(&key) {
                    self.ambiguous_cron_fns.contains(&key)
                } else {
                    self.ambiguous_inline_cron_fns.contains(&key)
                }
            },
            |inline| {
                self.ambiguous_scoped_cron_fns.contains(&(
                    file.to_owned(),
                    inline.to_owned(),
                    fn_name.to_owned(),
                ))
            },
        )
    }

    /// Index a function carrying a cron attribute and, when it is
    /// schedulable and `gate` (the function's and its modules' `#[cfg]`s) is
    /// not statically false, record it as a scanned job under `gate`. A
    /// statically dead declaration is neither indexed nor a job.
    pub(super) fn add_cron_fn(
        &mut self,
        function_name: &str,
        job: Option<&CronJob>,
        gate: &CfgGate,
        scope: &Scope<'_>,
    ) {
        if gate.dead {
            return;
        }
        let file_fn = (scope.file.to_owned(), function_name.to_owned());
        if scope.inline.is_empty() {
            index_first(
                &mut self.cron_fns,
                &mut self.ambiguous_cron_fns,
                file_fn,
                scope.module_path,
            );
        } else {
            index_first(
                &mut self.inline_cron_fns,
                &mut self.ambiguous_inline_cron_fns,
                file_fn,
                scope.module_path,
            );
        }
        let scoped_key = (
            scope.file.to_owned(),
            scope.inline.to_owned(),
            function_name.to_owned(),
        );
        index_first(
            &mut self.scoped_cron_fns,
            &mut self.ambiguous_scoped_cron_fns,
            scoped_key,
            scope.module_path,
        );
        if let Some((expression, options, cfg)) = job {
            self.jobs.push(CronMetadata {
                expression: expression.clone(),
                function_name: function_name.to_owned(),
                module_path: scope.module_path.to_owned(),
                file_path: scope.file.to_owned(),
                options: options.clone(),
                cfg: gate.predicates.iter().chain(cfg).cloned().collect(),
            });
        }
    }
}

/// Records `module_path` as the module of `key`: the first one wins, and a
/// later, different one marks `key` ambiguous.
fn index_first<K: Eq + Hash>(
    index: &mut HashMap<K, String>,
    ambiguous: &mut HashSet<K>,
    key: K,
    module_path: &str,
) {
    if index.get(&key).is_some_and(|first| first != module_path) {
        ambiguous.insert(key);
    } else {
        index.entry(key).or_insert_with(|| module_path.to_owned());
    }
}
