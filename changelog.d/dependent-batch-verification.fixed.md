An explicitly declared verification proposed after an edit can now run in the
same batch when every preceding mutation succeeded, its producer confirms no
repair is needed, and no user control is pending. Failed or uncertain edits and
unclassified process calls still wait for the next observation boundary.
