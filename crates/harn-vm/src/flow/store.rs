//! SQLite-backed Harn Flow atom DAG store.
//!
//! The store is intentionally narrow: atoms are append-only, parent edges are
//! indexed for DAG traversal, and state vectors track per-site clocks for
//! causal delta sync between replicas. It also implements [`VcsBackend`] so the
//! same flow shipping surface can use durable SQLite storage.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;

use super::backend::{AtomRef, FlowSlice, GitExportReceipt, ShipReceipt, VcsBackend};
use super::{Atom, AtomId, Intent, IntentId, Slice as DerivedSlice, SliceId, VcsBackendError};

const SQLITE_ATOM_REF_PREFIX: &str = "sqlite://atoms";
const SQLITE_SLICE_REF_PREFIX: &str = "sqlite://slices";

/// Per-site causal clock vector for one principal/persona stream.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateVector {
    clocks: BTreeMap<String, u64>,
}

impl StateVector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, site_id: impl Into<String>, clock: u64) {
        self.clocks.insert(site_id.into(), clock);
    }

    pub fn clock(&self, site_id: &str) -> u64 {
        self.clocks.get(site_id).copied().unwrap_or(0)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, u64)> {
        self.clocks
            .iter()
            .map(|(site_id, clock)| (site_id.as_str(), *clock))
    }
}

/// Atom plus the site clock needed to apply it to another replica.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtomDelta {
    pub atom: Atom,
    pub site_id: String,
    pub clock: u64,
}

/// SQLite-backed Flow store.
pub struct SqliteFlowStore {
    site_id: String,
    conn: Mutex<Connection>,
}

impl fmt::Debug for SqliteFlowStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqliteFlowStore")
            .field("site_id", &self.site_id)
            .finish_non_exhaustive()
    }
}

impl SqliteFlowStore {
    /// Open or create a store at `path` using `site_id` for locally emitted
    /// atoms.
    pub fn open(
        path: impl AsRef<Path>,
        site_id: impl Into<String>,
    ) -> Result<Self, VcsBackendError> {
        let site_id = normalize_site_id(site_id.into())?;
        let conn = Connection::open(path)?;
        initialize_schema(&conn)?;
        Ok(Self {
            site_id,
            conn: Mutex::new(conn),
        })
    }

    /// Create an in-memory store for tests and ephemeral callers.
    pub fn in_memory(site_id: impl Into<String>) -> Result<Self, VcsBackendError> {
        let site_id = normalize_site_id(site_id.into())?;
        let conn = Connection::open_in_memory()?;
        initialize_schema(&conn)?;
        Ok(Self {
            site_id,
            conn: Mutex::new(conn),
        })
    }

    pub fn site_id(&self) -> &str {
        &self.site_id
    }

    /// Persist multiple locally emitted atoms in one transaction.
    pub fn emit_atoms(&self, atoms: &[Atom]) -> Result<Vec<AtomRef>, VcsBackendError> {
        self.emit_atoms_inner(atoms, true)
    }

    /// Persist new atoms that the caller has already verified.
    ///
    /// This is intended for sync and benchmark hot paths that validate a batch
    /// once at the boundary, then measure storage throughput independently from
    /// signature verification cost. The caller must guarantee these atoms are
    /// not already present in the store.
    pub fn emit_preverified_atoms(&self, atoms: &[Atom]) -> Result<Vec<AtomRef>, VcsBackendError> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let mut clocks: HashMap<(String, String), u64> = HashMap::new();
        let mut refs = Vec::with_capacity(atoms.len());

        {
            let mut insert_atom = tx.prepare_cached(
                "INSERT INTO atoms (
                     id, principal, persona, timestamp_ns, timestamp_rfc3339,
                     site_id, site_clock, inverse_of, body_binary
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            let mut insert_parent = tx.prepare_cached(
                "INSERT INTO atom_parents (child_id, parent_id, ordinal)
                 VALUES (?1, ?2, ?3)",
            )?;

            for atom in atoms {
                let key = (
                    atom.provenance.principal.clone(),
                    atom.provenance.persona.clone(),
                );
                if !clocks.contains_key(&key) {
                    let current = state_vector_clock_tx(
                        &tx,
                        &atom.provenance.principal,
                        &atom.provenance.persona,
                        &self.site_id,
                    )?;
                    clocks.insert(key.clone(), current);
                }
                let clock = clocks
                    .get_mut(&key)
                    .expect("clock was inserted before increment");
                *clock = clock
                    .checked_add(1)
                    .ok_or_else(|| VcsBackendError::Invalid("site clock overflow".to_string()))?;

                let body = atom.to_binary()?;
                let timestamp_ns = atom_timestamp_ns(atom)?;
                let timestamp_rfc3339 = atom_timestamp_rfc3339(atom)?;
                let inverse_of = atom.inverse_of.map(|id| id.0.to_vec());
                insert_atom.execute(params![
                    atom.id.0.as_slice(),
                    atom.provenance.principal,
                    atom.provenance.persona,
                    timestamp_ns,
                    timestamp_rfc3339,
                    self.site_id.as_str(),
                    i64_from_u64(*clock, "atom site clock")?,
                    inverse_of.as_deref(),
                    body.as_slice(),
                ])?;

                for (ordinal, parent) in atom.parents.iter().enumerate() {
                    insert_parent.execute(params![
                        atom.id.0.as_slice(),
                        parent.0.as_slice(),
                        i64_from_usize(ordinal, "atom parent ordinal")?
                    ])?;
                }
                refs.push(sqlite_atom_ref(atom.id, &self.site_id, *clock));
            }
        }

        for ((principal, persona), clock) in clocks {
            advance_state_vector_tx(&tx, &principal, &persona, &self.site_id, clock)?;
        }
        tx.commit()?;
        Ok(refs)
    }

    fn emit_atoms_inner(
        &self,
        atoms: &[Atom],
        verify: bool,
    ) -> Result<Vec<AtomRef>, VcsBackendError> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let mut refs = Vec::with_capacity(atoms.len());
        for atom in atoms {
            if verify {
                atom.verify()?;
            }
            refs.push(insert_atom_tx(&tx, atom, &self.site_id, None)?);
        }
        tx.commit()?;
        Ok(refs)
    }

    /// Persist a remote atom at its original site clock.
    pub fn insert_remote_atom(
        &self,
        atom: &Atom,
        site_id: &str,
        clock: u64,
    ) -> Result<AtomRef, VcsBackendError> {
        atom.verify()?;
        if clock == 0 {
            return Err(VcsBackendError::Invalid(
                "remote atom clock must be greater than zero".to_string(),
            ));
        }
        let site_id = normalize_site_id(site_id.to_string())?;
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        let atom_ref = insert_atom_tx(&tx, atom, &site_id, Some(clock))?;
        tx.commit()?;
        Ok(atom_ref)
    }

    /// Load one atom by id.
    pub fn get_atom(&self, atom_id: AtomId) -> Result<Atom, VcsBackendError> {
        let conn = self.lock_conn()?;
        load_atom(&conn, atom_id)
    }

    /// Find an atom by its content hash. For Flow atoms the content hash is the
    /// atom id, so this uses the primary-key index directly.
    pub fn atom_by_content_hash(
        &self,
        content_hash: AtomId,
    ) -> Result<Option<Atom>, VcsBackendError> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT body_binary FROM atoms WHERE id = ?1",
            params![content_hash.0.as_slice()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .map(|body| Atom::from_binary_slice(&body).map_err(Into::into))
        .transpose()
    }

    /// Load atoms for a principal/persona ordered by timestamp and atom id.
    pub fn atoms_for_principal_persona(
        &self,
        principal: &str,
        persona: &str,
    ) -> Result<Vec<Atom>, VcsBackendError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id FROM atoms
             WHERE principal = ?1 AND persona = ?2
             ORDER BY timestamp_ns, id",
        )?;
        let rows = stmt.query_map(params![principal, persona], |row| row.get::<_, Vec<u8>>(0))?;
        let mut atoms = Vec::new();
        for row in rows {
            atoms.push(load_atom(&conn, atom_id_from_blob(row?)?)?);
        }
        Ok(atoms)
    }

    /// Count atoms for a principal/persona using the timestamp index.
    pub fn atom_count_for_principal_persona(
        &self,
        principal: &str,
        persona: &str,
    ) -> Result<u64, VcsBackendError> {
        let conn = self.lock_conn()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM atoms WHERE principal = ?1 AND persona = ?2",
            params![principal, persona],
            |row| row.get::<_, i64>(0),
        )?;
        u64_from_i64(count, "atom count")
    }

    /// Load all child atoms that list `parent` as a parent edge.
    pub fn atoms_with_parent(&self, parent: AtomId) -> Result<Vec<Atom>, VcsBackendError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT child_id FROM atom_parents
             WHERE parent_id = ?1
             ORDER BY child_id",
        )?;
        let rows = stmt.query_map(params![parent.0.as_slice()], |row| row.get::<_, Vec<u8>>(0))?;
        let mut atoms = Vec::new();
        for row in rows {
            atoms.push(load_atom(&conn, atom_id_from_blob(row?)?)?);
        }
        Ok(atoms)
    }

    /// Current state vector for one principal/persona stream.
    pub fn state_vector(
        &self,
        principal: &str,
        persona: &str,
    ) -> Result<StateVector, VcsBackendError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT site_id, clock FROM state_vectors
             WHERE principal = ?1 AND persona = ?2
             ORDER BY site_id",
        )?;
        let rows = stmt.query_map(params![principal, persona], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut vector = StateVector::new();
        for row in rows {
            let (site_id, clock) = row?;
            vector.insert(site_id, u64_from_i64(clock, "state vector clock")?);
        }
        Ok(vector)
    }

    /// Return atoms this store has that are newer than `remote`.
    pub fn causal_delta(
        &self,
        principal: &str,
        persona: &str,
        remote: &StateVector,
    ) -> Result<Vec<AtomDelta>, VcsBackendError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, site_id, site_clock FROM atoms
             WHERE principal = ?1 AND persona = ?2
             ORDER BY site_id, site_clock, id",
        )?;
        let rows = stmt.query_map(params![principal, persona], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut delta = Vec::new();
        for row in rows {
            let (id_blob, site_id, clock_raw) = row?;
            let clock = u64_from_i64(clock_raw, "atom site clock")?;
            if clock > remote.clock(&site_id) {
                delta.push(AtomDelta {
                    atom: load_atom(&conn, atom_id_from_blob(id_blob)?)?,
                    site_id,
                    clock,
                });
            }
        }
        Ok(delta)
    }

    /// Persist an intent record and its atom edges.
    pub fn put_intent(&self, intent: &Intent) -> Result<(), VcsBackendError> {
        let body = serde_json::to_vec(intent)?;
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO intents (id, body_json, goal_description, confidence)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                intent.id.0.as_slice(),
                body.as_slice(),
                intent.goal_description,
                f64::from(intent.confidence)
            ],
        )?;
        for (ordinal, atom_id) in intent.atoms.iter().enumerate() {
            tx.execute(
                "INSERT OR IGNORE INTO intent_atoms (intent_id, atom_id, ordinal)
                 VALUES (?1, ?2, ?3)",
                params![
                    intent.id.0.as_slice(),
                    atom_id.0.as_slice(),
                    i64_from_usize(ordinal, "intent atom ordinal")?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_intent(&self, intent_id: IntentId) -> Result<Intent, VcsBackendError> {
        let conn = self.lock_conn()?;
        let body = conn
            .query_row(
                "SELECT body_json FROM intents WHERE id = ?1",
                params![intent_id.0.as_slice()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?
            .ok_or_else(|| VcsBackendError::NotFound(format!("intent {intent_id} not found")))?;
        serde_json::from_slice(&body).map_err(Into::into)
    }

    /// Persist a derived Flow slice record.
    pub fn put_derived_slice(&self, slice: &DerivedSlice) -> Result<(), VcsBackendError> {
        let body = serde_json::to_vec(slice)?;
        self.insert_slice_record(slice.id, &slice.atoms, "derived", body, false)
    }

    pub fn get_derived_slice(&self, slice_id: SliceId) -> Result<DerivedSlice, VcsBackendError> {
        let conn = self.lock_conn()?;
        let body = conn
            .query_row(
                "SELECT body_json FROM slices WHERE id = ?1 AND slice_kind = 'derived'",
                params![slice_id.0.as_slice()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?
            .ok_or_else(|| VcsBackendError::NotFound(format!("slice {slice_id} not found")))?;
        serde_json::from_slice(&body).map_err(Into::into)
    }

    fn insert_flow_slice(&self, slice: &FlowSlice, shipped: bool) -> Result<(), VcsBackendError> {
        let body = serde_json::to_vec(slice)?;
        self.insert_slice_record(slice.id, &slice.atoms, "flow", body, shipped)
    }

    fn insert_slice_record(
        &self,
        slice_id: SliceId,
        atoms: &[AtomId],
        kind: &str,
        body: Vec<u8>,
        shipped: bool,
    ) -> Result<(), VcsBackendError> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        insert_slice_record_tx(&tx, slice_id, atoms, kind, &body, shipped)?;
        tx.commit()?;
        Ok(())
    }

    fn atom_closure(&self, roots: &[AtomId]) -> Result<Vec<AtomId>, VcsBackendError> {
        let mut opened = HashSet::new();
        let mut emitted = HashSet::new();
        let mut out = Vec::new();
        let mut stack: Vec<(AtomId, bool)> = roots
            .iter()
            .rev()
            .copied()
            .map(|atom_id| (atom_id, false))
            .collect();

        while let Some((atom_id, emit)) = stack.pop() {
            if emit {
                if emitted.insert(atom_id) {
                    out.push(atom_id);
                }
                continue;
            }
            if emitted.contains(&atom_id) || !opened.insert(atom_id) {
                continue;
            }

            let atom = self.get_atom(atom_id)?;
            stack.push((atom_id, true));
            for parent in atom.parents.iter().rev() {
                if !emitted.contains(parent) {
                    stack.push((*parent, false));
                }
            }
        }

        Ok(out)
    }

    fn lock_conn(&self) -> Result<MutexGuard<'_, Connection>, VcsBackendError> {
        self.conn
            .lock()
            .map_err(|_| VcsBackendError::Io("sqlite flow store lock poisoned".to_string()))
    }
}

impl VcsBackend for SqliteFlowStore {
    fn emit_atom(&self, atom: &Atom) -> Result<AtomRef, VcsBackendError> {
        self.emit_atoms(std::slice::from_ref(atom))
            .map(|mut refs| refs.remove(0))
    }

    fn derive_slice(&self, atoms: &[AtomId]) -> Result<FlowSlice, VcsBackendError> {
        FlowSlice::new(self.atom_closure(atoms)?)
    }

    fn ship_slice(&self, slice: &FlowSlice) -> Result<ShipReceipt, VcsBackendError> {
        self.insert_flow_slice(slice, true)?;
        Ok(ShipReceipt {
            slice_id: slice.id,
            commit: slice.id.to_string(),
            ref_name: format!("{SQLITE_SLICE_REF_PREFIX}/{}", slice.id),
        })
    }

    fn list_atoms(&self) -> Result<Vec<AtomRef>, VcsBackendError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, site_id, site_clock FROM atoms
             ORDER BY principal, persona, timestamp_ns, id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut atoms = Vec::new();
        for row in rows {
            let (id_blob, site_id, clock_raw) = row?;
            atoms.push(sqlite_atom_ref(
                atom_id_from_blob(id_blob)?,
                &site_id,
                u64_from_i64(clock_raw, "atom site clock")?,
            ));
        }
        Ok(atoms)
    }

    fn replay_slice(&self, slice: &FlowSlice) -> Result<Vec<Atom>, VcsBackendError> {
        slice
            .atoms
            .iter()
            .map(|atom_id| self.get_atom(*atom_id))
            .collect()
    }

    fn export_git(
        &self,
        _slice: &FlowSlice,
        _ref_name: &str,
    ) -> Result<GitExportReceipt, VcsBackendError> {
        Err(VcsBackendError::Unsupported(
            "SqliteFlowStore cannot export git refs; use ShadowGitBackend for git export"
                .to_string(),
        ))
    }

    fn import_git(&self, _ref_name: &str) -> Result<FlowSlice, VcsBackendError> {
        Err(VcsBackendError::Unsupported(
            "SqliteFlowStore cannot import git refs; use ShadowGitBackend for git import"
                .to_string(),
        ))
    }
}

fn initialize_schema(conn: &Connection) -> Result<(), VcsBackendError> {
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;

        CREATE TABLE IF NOT EXISTS atoms (
            id BLOB PRIMARY KEY CHECK(length(id) = 32),
            principal TEXT NOT NULL,
            persona TEXT NOT NULL,
            timestamp_ns INTEGER NOT NULL,
            timestamp_rfc3339 TEXT NOT NULL,
            site_id TEXT NOT NULL,
            site_clock INTEGER NOT NULL CHECK(site_clock > 0),
            inverse_of BLOB CHECK(inverse_of IS NULL OR length(inverse_of) = 32),
            body_binary BLOB NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(principal, persona, site_id, site_clock)
        );

        CREATE INDEX IF NOT EXISTS atoms_principal_persona_timestamp_idx
            ON atoms(principal, persona, timestamp_ns, id);
        CREATE INDEX IF NOT EXISTS atoms_principal_persona_site_clock_idx
            ON atoms(principal, persona, site_id, site_clock);
        CREATE INDEX IF NOT EXISTS atoms_inverse_of_idx ON atoms(inverse_of);

        CREATE TABLE IF NOT EXISTS atom_parents (
            child_id BLOB NOT NULL CHECK(length(child_id) = 32),
            parent_id BLOB NOT NULL CHECK(length(parent_id) = 32),
            ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
            PRIMARY KEY(child_id, ordinal),
            UNIQUE(child_id, parent_id),
            FOREIGN KEY(child_id) REFERENCES atoms(id)
        );
        CREATE INDEX IF NOT EXISTS atom_parents_parent_idx
            ON atom_parents(parent_id, child_id);

        CREATE TABLE IF NOT EXISTS intents (
            id BLOB PRIMARY KEY CHECK(length(id) = 32),
            body_json BLOB NOT NULL,
            goal_description TEXT NOT NULL,
            confidence REAL NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS intent_atoms (
            intent_id BLOB NOT NULL CHECK(length(intent_id) = 32),
            atom_id BLOB NOT NULL CHECK(length(atom_id) = 32),
            ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
            PRIMARY KEY(intent_id, ordinal),
            UNIQUE(intent_id, atom_id),
            FOREIGN KEY(intent_id) REFERENCES intents(id)
        );
        CREATE INDEX IF NOT EXISTS intent_atoms_atom_idx
            ON intent_atoms(atom_id, intent_id);

        CREATE TABLE IF NOT EXISTS slices (
            id BLOB PRIMARY KEY CHECK(length(id) = 32),
            slice_kind TEXT NOT NULL,
            body_json BLOB NOT NULL,
            shipped INTEGER NOT NULL DEFAULT 0,
            ref_name TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS slice_atoms (
            slice_id BLOB NOT NULL CHECK(length(slice_id) = 32),
            atom_id BLOB NOT NULL CHECK(length(atom_id) = 32),
            ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
            PRIMARY KEY(slice_id, ordinal),
            UNIQUE(slice_id, atom_id),
            FOREIGN KEY(slice_id) REFERENCES slices(id)
        );
        CREATE INDEX IF NOT EXISTS slice_atoms_atom_idx
            ON slice_atoms(atom_id, slice_id);

        CREATE TABLE IF NOT EXISTS state_vectors (
            principal TEXT NOT NULL,
            persona TEXT NOT NULL,
            site_id TEXT NOT NULL,
            clock INTEGER NOT NULL CHECK(clock >= 0),
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY(principal, persona, site_id)
        );

        CREATE TRIGGER IF NOT EXISTS atoms_no_update
        BEFORE UPDATE ON atoms
        BEGIN
            SELECT RAISE(ABORT, 'atoms are append-only');
        END;

        CREATE TRIGGER IF NOT EXISTS atoms_no_delete
        BEFORE DELETE ON atoms
        BEGIN
            SELECT RAISE(ABORT, 'atoms are append-only');
        END;

        CREATE TRIGGER IF NOT EXISTS atom_parents_no_update
        BEFORE UPDATE ON atom_parents
        BEGIN
            SELECT RAISE(ABORT, 'atom parent edges are append-only');
        END;

        CREATE TRIGGER IF NOT EXISTS atom_parents_no_delete
        BEFORE DELETE ON atom_parents
        BEGIN
            SELECT RAISE(ABORT, 'atom parent edges are append-only');
        END;
        "#,
    )?;
    Ok(())
}

fn insert_atom_tx(
    tx: &Transaction<'_>,
    atom: &Atom,
    site_id: &str,
    explicit_clock: Option<u64>,
) -> Result<AtomRef, VcsBackendError> {
    if let Some((existing_site, existing_clock)) = atom_clock_tx(tx, atom.id)? {
        return Ok(sqlite_atom_ref(atom.id, &existing_site, existing_clock));
    }

    let clock = match explicit_clock {
        Some(clock) => {
            reject_site_clock_conflict(
                tx,
                &atom.provenance.principal,
                &atom.provenance.persona,
                site_id,
                clock,
                atom.id,
            )?;
            advance_state_vector_tx(
                tx,
                &atom.provenance.principal,
                &atom.provenance.persona,
                site_id,
                clock,
            )?;
            clock
        }
        None => reserve_next_clock_tx(
            tx,
            &atom.provenance.principal,
            &atom.provenance.persona,
            site_id,
        )?,
    };

    let body = atom.to_binary()?;
    let timestamp_ns = atom_timestamp_ns(atom)?;
    let timestamp_rfc3339 = atom_timestamp_rfc3339(atom)?;
    let inverse_of = atom.inverse_of.map(|id| id.0.to_vec());
    tx.execute(
        "INSERT INTO atoms (
             id, principal, persona, timestamp_ns, timestamp_rfc3339,
             site_id, site_clock, inverse_of, body_binary
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            atom.id.0.as_slice(),
            atom.provenance.principal,
            atom.provenance.persona,
            timestamp_ns,
            timestamp_rfc3339,
            site_id,
            i64_from_u64(clock, "atom site clock")?,
            inverse_of.as_deref(),
            body.as_slice(),
        ],
    )?;

    for (ordinal, parent) in atom.parents.iter().enumerate() {
        tx.execute(
            "INSERT INTO atom_parents (child_id, parent_id, ordinal)
             VALUES (?1, ?2, ?3)",
            params![
                atom.id.0.as_slice(),
                parent.0.as_slice(),
                i64_from_usize(ordinal, "atom parent ordinal")?
            ],
        )?;
    }

    Ok(sqlite_atom_ref(atom.id, site_id, clock))
}

fn insert_slice_record_tx(
    tx: &Transaction<'_>,
    slice_id: SliceId,
    atoms: &[AtomId],
    kind: &str,
    body: &[u8],
    shipped: bool,
) -> Result<(), VcsBackendError> {
    tx.execute(
        "INSERT OR IGNORE INTO slices (id, slice_kind, body_json, shipped, ref_name)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            slice_id.0.as_slice(),
            kind,
            body,
            if shipped { 1 } else { 0 },
            if shipped {
                Some(format!("{SQLITE_SLICE_REF_PREFIX}/{slice_id}"))
            } else {
                None
            }
        ],
    )?;
    for (ordinal, atom_id) in atoms.iter().enumerate() {
        tx.execute(
            "INSERT OR IGNORE INTO slice_atoms (slice_id, atom_id, ordinal)
             VALUES (?1, ?2, ?3)",
            params![
                slice_id.0.as_slice(),
                atom_id.0.as_slice(),
                i64_from_usize(ordinal, "slice atom ordinal")?
            ],
        )?;
    }
    Ok(())
}

fn load_atom(conn: &Connection, atom_id: AtomId) -> Result<Atom, VcsBackendError> {
    let body = conn
        .query_row(
            "SELECT body_binary FROM atoms WHERE id = ?1",
            params![atom_id.0.as_slice()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .ok_or_else(|| VcsBackendError::NotFound(format!("atom {atom_id} not found")))?;
    Atom::from_binary_slice(&body).map_err(Into::into)
}

fn atom_clock_tx(
    tx: &Transaction<'_>,
    atom_id: AtomId,
) -> Result<Option<(String, u64)>, VcsBackendError> {
    tx.query_row(
        "SELECT site_id, site_clock FROM atoms WHERE id = ?1",
        params![atom_id.0.as_slice()],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    )
    .optional()?
    .map(|(site_id, clock)| Ok((site_id, u64_from_i64(clock, "atom site clock")?)))
    .transpose()
}

fn reserve_next_clock_tx(
    tx: &Transaction<'_>,
    principal: &str,
    persona: &str,
    site_id: &str,
) -> Result<u64, VcsBackendError> {
    let current = state_vector_clock_tx(tx, principal, persona, site_id)?;
    let next = current
        .checked_add(1)
        .ok_or_else(|| VcsBackendError::Invalid("state vector clock overflow".to_string()))?;
    advance_state_vector_tx(tx, principal, persona, site_id, next)?;
    Ok(next)
}

fn state_vector_clock_tx(
    tx: &Transaction<'_>,
    principal: &str,
    persona: &str,
    site_id: &str,
) -> Result<u64, VcsBackendError> {
    tx.query_row(
        "SELECT clock FROM state_vectors
         WHERE principal = ?1 AND persona = ?2 AND site_id = ?3",
        params![principal, persona, site_id],
        |row| row.get::<_, i64>(0),
    )
    .optional()?
    .map(|clock| u64_from_i64(clock, "state vector clock"))
    .transpose()
    .map(|clock| clock.unwrap_or(0))
}

fn advance_state_vector_tx(
    tx: &Transaction<'_>,
    principal: &str,
    persona: &str,
    site_id: &str,
    clock: u64,
) -> Result<(), VcsBackendError> {
    tx.execute(
        "INSERT INTO state_vectors (principal, persona, site_id, clock, updated_at)
         VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)
         ON CONFLICT(principal, persona, site_id) DO UPDATE SET
             clock = CASE
                 WHEN excluded.clock > state_vectors.clock THEN excluded.clock
                 ELSE state_vectors.clock
             END,
             updated_at = CURRENT_TIMESTAMP",
        params![
            principal,
            persona,
            site_id,
            i64_from_u64(clock, "state vector clock")?
        ],
    )?;
    Ok(())
}

fn reject_site_clock_conflict(
    tx: &Transaction<'_>,
    principal: &str,
    persona: &str,
    site_id: &str,
    clock: u64,
    atom_id: AtomId,
) -> Result<(), VcsBackendError> {
    let existing = tx
        .query_row(
            "SELECT id FROM atoms
             WHERE principal = ?1 AND persona = ?2 AND site_id = ?3 AND site_clock = ?4",
            params![
                principal,
                persona,
                site_id,
                i64_from_u64(clock, "atom site clock")?
            ],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        let existing = atom_id_from_blob(existing)?;
        if existing != atom_id {
            return Err(VcsBackendError::Invalid(format!(
                "site clock conflict for {site_id}@{clock}: existing atom {existing}, new atom {atom_id}"
            )));
        }
    }
    Ok(())
}

fn sqlite_atom_ref(atom_id: AtomId, site_id: &str, clock: u64) -> AtomRef {
    AtomRef {
        atom_id,
        commit: format!("{site_id}:{clock}"),
        ref_name: format!("{SQLITE_ATOM_REF_PREFIX}/{atom_id}"),
    }
}

fn atom_timestamp_ns(atom: &Atom) -> Result<i64, VcsBackendError> {
    i64::try_from(atom.provenance.timestamp.unix_timestamp_nanos())
        .map_err(|_| VcsBackendError::Invalid("atom timestamp is out of SQLite range".to_string()))
}

fn atom_timestamp_rfc3339(atom: &Atom) -> Result<String, VcsBackendError> {
    atom.provenance
        .timestamp
        .format(&Rfc3339)
        .map_err(|error| VcsBackendError::Invalid(format!("atom timestamp format: {error}")))
}

fn atom_id_from_blob(blob: Vec<u8>) -> Result<AtomId, VcsBackendError> {
    if blob.len() != 32 {
        return Err(VcsBackendError::Invalid(format!(
            "atom id blob must be 32 bytes, got {}",
            blob.len()
        )));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&blob);
    Ok(AtomId(out))
}

fn normalize_site_id(site_id: String) -> Result<String, VcsBackendError> {
    if site_id.trim().is_empty() {
        return Err(VcsBackendError::Invalid(
            "flow store site_id must not be empty".to_string(),
        ));
    }
    Ok(site_id)
}

fn i64_from_u64(value: u64, field: &str) -> Result<i64, VcsBackendError> {
    i64::try_from(value)
        .map_err(|_| VcsBackendError::Invalid(format!("{field} exceeds SQLite i64 range")))
}

fn i64_from_usize(value: usize, field: &str) -> Result<i64, VcsBackendError> {
    i64::try_from(value)
        .map_err(|_| VcsBackendError::Invalid(format!("{field} exceeds SQLite i64 range")))
}

fn u64_from_i64(value: i64, field: &str) -> Result<u64, VcsBackendError> {
    u64::try_from(value).map_err(|_| VcsBackendError::Invalid(format!("{field} is negative")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{Approval, CoverageMap, PredicateHash, Slice, SliceStatus, TestId, TextOp};
    use ed25519_dalek::SigningKey;
    use time::OffsetDateTime;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn atom(index: u64, parents: Vec<AtomId>) -> Atom {
        let principal = key(1);
        let persona = key(2);
        let timestamp = OffsetDateTime::from_unix_timestamp(1_775_000_000 + index as i64).unwrap();
        Atom::sign(
            vec![TextOp::Insert {
                offset: index,
                content: format!("atom-{index}"),
            }],
            parents,
            crate::flow::Provenance {
                principal: "user:alice".to_string(),
                persona: "ship-captain".to_string(),
                agent_run_id: format!("run-{index}"),
                tool_call_id: Some(format!("tool-{index}")),
                trace_id: "trace-1".to_string(),
                transcript_ref: "transcript-1".to_string(),
                timestamp,
            },
            None,
            &principal,
            &persona,
        )
        .unwrap()
    }

    #[test]
    fn emits_replays_and_queries_atoms() {
        let store = SqliteFlowStore::in_memory("site-a").unwrap();
        let first = atom(1, vec![]);
        let second = atom(2, vec![first.id]);

        let refs = store.emit_atoms(&[first.clone(), second.clone()]).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].commit, "site-a:1");
        assert_eq!(refs[1].commit, "site-a:2");
        assert_eq!(store.get_atom(first.id).unwrap(), first);
        assert_eq!(
            store.atom_by_content_hash(second.id).unwrap(),
            Some(second.clone())
        );
        assert_eq!(
            store.atoms_with_parent(first.id).unwrap(),
            vec![second.clone()]
        );
        assert_eq!(
            store
                .atoms_for_principal_persona("user:alice", "ship-captain")
                .unwrap(),
            vec![first, second]
        );
    }

    #[test]
    fn derives_and_replays_parent_closed_slices() {
        let store = SqliteFlowStore::in_memory("site-a").unwrap();
        let first = atom(1, vec![]);
        let second = atom(2, vec![first.id]);
        store.emit_atoms(&[first.clone(), second.clone()]).unwrap();

        let slice = store.derive_slice(&[second.id]).unwrap();
        assert_eq!(slice.atoms, vec![first.id, second.id]);
        let receipt = store.ship_slice(&slice).unwrap();
        assert_eq!(receipt.slice_id, slice.id);
        assert_eq!(store.replay_slice(&slice).unwrap(), vec![first, second]);
    }

    #[test]
    fn state_vector_delta_round_trips_between_replicas() {
        let source = SqliteFlowStore::in_memory("site-a").unwrap();
        let replica = SqliteFlowStore::in_memory("site-b").unwrap();
        let first = atom(1, vec![]);
        let second = atom(2, vec![first.id]);
        source.emit_atoms(&[first.clone(), second.clone()]).unwrap();

        let empty = replica.state_vector("user:alice", "ship-captain").unwrap();
        let delta = source
            .causal_delta("user:alice", "ship-captain", &empty)
            .unwrap();
        assert_eq!(delta.len(), 2);
        for item in &delta {
            replica
                .insert_remote_atom(&item.atom, &item.site_id, item.clock)
                .unwrap();
        }

        let vector = replica.state_vector("user:alice", "ship-captain").unwrap();
        assert_eq!(vector.clock("site-a"), 2);
        assert!(source
            .causal_delta("user:alice", "ship-captain", &vector)
            .unwrap()
            .is_empty());
        assert_eq!(replica.get_atom(second.id).unwrap(), second);
    }

    #[test]
    fn persists_intents_and_derived_slices() {
        let store = SqliteFlowStore::in_memory("site-a").unwrap();
        let first = atom(1, vec![]);
        store.emit_atom(&first).unwrap();

        let intent = Intent::new(
            vec![first.id],
            "ship the smallest possible change",
            crate::flow::TranscriptSpan::new("transcript-1", 1, 1).unwrap(),
            0.9,
        )
        .unwrap();
        store.put_intent(&intent).unwrap();
        assert_eq!(store.get_intent(intent.id).unwrap(), intent);

        let mut coverage = CoverageMap::new();
        coverage.insert(first.id, TestId::new("flow-store"));
        let slice = Slice {
            id: SliceId([3; 32]),
            atoms: vec![first.id],
            intents: vec![intent.id],
            invariants_applied: vec![(
                PredicateHash::new("pred"),
                crate::flow::InvariantResult::allow(),
            )],
            required_tests: vec![TestId::new("flow-store")],
            approval_chain: vec![Approval {
                reviewer: "alice".to_string(),
                approved_at: "2026-04-25T00:00:00Z".to_string(),
                reason: None,
                signature: None,
            }],
            base_ref: first.id,
            status: SliceStatus::Ready,
        };
        store.put_derived_slice(&slice).unwrap();
        assert_eq!(store.get_derived_slice(slice.id).unwrap(), slice);
    }

    #[test]
    fn atoms_are_append_only_at_sql_boundary() {
        let store = SqliteFlowStore::in_memory("site-a").unwrap();
        let first = atom(1, vec![]);
        store.emit_atom(&first).unwrap();

        let conn = store.lock_conn().unwrap();
        let error = conn
            .execute(
                "DELETE FROM atoms WHERE id = ?1",
                params![first.id.0.as_slice()],
            )
            .unwrap_err();
        assert!(error.to_string().contains("atoms are append-only"));
    }
}
