Library crates no longer force `harn-vm/full`, and `harn-hostlib`'s `ast`
feature no longer pulls every tree-sitter grammar — lean in-process embedders
can drop sqlx/AWS/content parsers and select grammar families without Cargo
unifying the fat surface back on.
