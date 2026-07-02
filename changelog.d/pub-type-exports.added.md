- `pub type` exports a type alias from a module. Importers can name it in
  selective imports (`import { SmartTarget, pick } from "./targets"`), use it
  in annotations, and pass it in schema positions (`output_schema:`,
  `schema_is`) — the loader binds the imported name to the alias's JSON-Schema
  lowering, and `pub import` re-exports it through facades. Type aliases
  without `pub` stay module-private and error on import, matching `pub fn` /
  `pub struct` visibility.
