import { formatDate, parseQuery } from "./util";

export async function fetchUser(id: string): Promise<{ id: string; updated: string }> {
    const params = parseQuery(`id=${id}`);
    const url = `/users/${params.id}`;
    const res = await fetch(url);
    const body = await res.json();
    body.updated = formatDate(new Date());
    return body;
}

export function buildHeaders(token: string): Record<string, string> {
    return { authorization: `Bearer ${token}` };
}
