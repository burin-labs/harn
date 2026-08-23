// The markup contract for a code block that names its source file, shared by
// the build-time pipeline that emits it (vite-plugins/content.ts), the tests
// that assert on it, and index.css, which styles the same class names.
//
// A fence opts in with `title`:
//
//     ```harn title="example.harn"
//
// which becomes a <figure> wrapping the <pre>, with the filename as a real
// <figcaption> so the association survives for assistive technology.
export const CODE_FIGURE_CLASS = "code-figure"
export const CODE_FILENAME_CLASS = "code-filename"
