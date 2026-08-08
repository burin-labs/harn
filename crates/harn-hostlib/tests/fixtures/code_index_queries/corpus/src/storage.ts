import { pad } from "./util";

export function saveSession(token: string): void {
    const padded = pad(token, 32);
    localStorage.setItem("session", padded);
}

export function loadSession(): string | null {
    return localStorage.getItem("session");
}

export function clearSession(): void {
    localStorage.removeItem("session");
}
