export class User {
    constructor(public id: string, public name: string) {}

    greet(): string {
        return `hi, ${this.name}`;
    }
}

export class Session {
    constructor(public token: string, public expires: number) {}

    isExpired(): boolean {
        return Date.now() > this.expires;
    }
}

export interface Repository<T> {
    get(id: string): Promise<T | null>;
    save(value: T): Promise<void>;
}
