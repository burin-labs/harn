pub(super) struct OwnerJournalCleanup(pub(super) String);

impl Drop for OwnerJournalCleanup {
    fn drop(&mut self) {
        harn_vm::op_interrupt::remove_process_owner_group_journal(&self.0);
    }
}
