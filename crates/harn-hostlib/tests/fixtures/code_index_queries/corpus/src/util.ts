export function formatDate(d: Date): string {
    return d.toISOString();
}

export function parseQuery(qs: string): Record<string, string> {
    const out: Record<string, string> = {};
    for (const part of qs.split("&")) {
        const [k, v] = part.split("=");
        out[k] = v ?? "";
    }
    return out;
}

export function pad(value: string, width: number): string {
    return value.padStart(width, "0");
}
