import { fetchUser, buildHeaders } from "./api";
import { saveSession, loadSession, clearSession } from "./storage";

export async function login(id: string, token: string): Promise<void> {
    const headers = buildHeaders(token);
    await fetchUser(id);
    saveSession(token);
    const _ = headers;
}

export function logout(): void {
    clearSession();
    const previous = loadSession();
    void previous;
}
