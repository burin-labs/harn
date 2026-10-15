use super::*;

impl PreparedModuleCache {
    pub(super) fn prepare_graph_with_provenance(
        &self,
        roots: &[PathBuf],
        provenance: ModuleProvenance,
        include_roots: bool,
    ) -> Result<ModulePhaseStats, VmError> {
        if roots.is_empty() {
            return Ok(ModulePhaseStats::default());
        }

        // This walk reads every reachable file, so the interfaces it derives
        // supersede anything remembered from an earlier generation of the same
        // tree.
        self.interfaces
            .lock()
            .expect("interface memo lock poisoned")
            .clear();
        self.sources
            .lock()
            .expect("prepared-module source lock poisoned")
            .clear();
        let (graph, mut projected_sources) =
            harn_modules::build_with_module_projection(roots, |_path, source, program| {
                (
                    Arc::new(ModuleSource::from_text(source)),
                    program.map(ModuleCompilationContext::project_program),
                )
            });
        let root_paths = roots
            .iter()
            .map(|path| harn_modules::canonical_path(path))
            .collect::<std::collections::HashSet<_>>();
        let recorder = ModulePhaseRecorder::new();
        let validation = PreparedModuleValidation::default();

        for path in graph.module_paths() {
            if !include_roots && root_paths.contains(&harn_modules::canonical_path(&path)) {
                continue;
            }
            if path.to_str().is_some_and(|path| path.starts_with("<std>/")) {
                let _ = crate::vm::prepare_stdlib_module_artifact(&path, Some(&recorder));
                continue;
            }

            let (source, projection) = match projected_sources.remove(&path) {
                Some(projected) => projected,
                None => {
                    // Unreadable graph inputs retain the canonical read error
                    // for generations and best-effort import prewarming.
                    let source = {
                        let _load_span = recorder.load_span();
                        match crate::module_source::read(&path) {
                            Ok(source) => source,
                            Err(error) if include_roots => {
                                return Err(VmError::Runtime(format!(
                                    "cannot prepare module generation source {}: {error}",
                                    path.display()
                                )))
                            }
                            Err(_) => continue,
                        }
                    };
                    (source, None)
                }
            };
            let compilation_context = match projection {
                Some(projection) => projection.in_graph(&graph, &path),
                None => {
                    // Reproduce canonical diagnostics from the graph's invalid
                    // bytes, without consulting a possibly stale source memo.
                    match ModuleCompilationContext::for_source_in_graph(
                        &graph,
                        &path,
                        source.as_str(),
                    ) {
                        Ok(context) => context,
                        Err(error) if include_roots => return Err(error),
                        Err(_) => continue,
                    }
                }
            };
            let canonical = harn_modules::canonical_path(&path);
            self.sources
                .lock()
                .expect("prepared-module source lock poisoned")
                .insert(canonical.clone(), Arc::clone(source.text()));
            let prepared = self.prepare(
                &path,
                &canonical,
                &source,
                Some(&compilation_context),
                Some(&recorder),
                provenance,
                &validation,
            );
            if include_roots {
                prepared?;
            }
        }

        if include_roots {
            // A root may be spelled through a filesystem alias (for example,
            // macOS `/var` and `/private/var`). Runtime relative imports retain
            // the caller's root spelling after the source files disappear, so
            // seed that lexical spelling beside the canonical graph identity.
            // Both names point at the same immutable bytes.
            let mut sources = self
                .sources
                .lock()
                .expect("prepared-module source lock poisoned");
            let canonical_sources = sources
                .iter()
                .map(|(path, source)| (path.clone(), Arc::clone(source)))
                .collect::<Vec<_>>();
            for root in roots {
                let Some(raw_parent) = root.parent() else {
                    continue;
                };
                let canonical_root = harn_modules::canonical_path(root);
                let Some(canonical_parent) = canonical_root.parent() else {
                    continue;
                };
                for (path, source) in &canonical_sources {
                    if let Ok(relative) = path.strip_prefix(canonical_parent) {
                        sources
                            .entry(raw_parent.join(relative))
                            .or_insert_with(|| Arc::clone(source));
                    }
                }
            }
        }

        if include_roots {
            // A complete generation is immutable by construction. Its source
            // snapshot, interfaces, and artifacts were captured by this one
            // graph walk and are swapped together by the owning host. Do not
            // revalidate those interfaces against a later filesystem state on
            // every invocation; a watcher prepares a replacement generation
            // when source changes.
            for entry in self
                .interfaces
                .lock()
                .expect("interface memo lock poisoned")
                .values_mut()
            {
                entry.manifest = None;
            }
        }

        Ok(recorder.snapshot())
    }
}
