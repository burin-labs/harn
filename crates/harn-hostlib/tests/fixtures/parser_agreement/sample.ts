import { readFile } from 'fs';
import path from "path";

export function load(p: string): void {
  readFile(path.resolve(p), () => {});
}
