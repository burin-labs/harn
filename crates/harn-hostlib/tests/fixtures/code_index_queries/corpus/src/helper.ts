export function debounce<T extends (...a: unknown[]) => void>(fn: T, wait: number): T {
    let handle: number | undefined;
    return ((...args: unknown[]) => {
        if (handle !== undefined) clearTimeout(handle);
        handle = setTimeout(() => fn(...args), wait) as unknown as number;
    }) as T;
}

export function throttle<T extends (...a: unknown[]) => void>(fn: T, wait: number): T {
    let last = 0;
    return ((...args: unknown[]) => {
        const now = Date.now();
        if (now - last >= wait) {
            last = now;
            fn(...args);
        }
    }) as T;
}
