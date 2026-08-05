import { route } from "./router";

export async function main(): Promise<void> {
    await route("/login");
    await route("/user/u1");
    await route("/logout");
}
