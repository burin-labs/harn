import { login, logout } from "./auth";
import { fetchUser } from "./api";

export async function route(path: string): Promise<string> {
    if (path === "/login") {
        await login("u1", "tok");
        return "ok";
    }
    if (path === "/logout") {
        logout();
        return "bye";
    }
    if (path.startsWith("/user/")) {
        const id = path.slice("/user/".length);
        const user = await fetchUser(id);
        return JSON.stringify(user);
    }
    return "not found";
}
