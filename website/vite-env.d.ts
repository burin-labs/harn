/// <reference types="vite/client" />

declare module "virtual:harn-docs" {
  import type { NavSection, DocMeta } from "./vite-plugins/content"
  export const nav: NavSection[]
  export const meta: Record<string, DocMeta>
}
