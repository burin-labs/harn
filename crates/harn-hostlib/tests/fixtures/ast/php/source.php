<?php

class Greeter {
    private string $name;

    public function __construct(string $name) {
        $this->name = $name;
    }

    public function greet(): string {
        return "hello " . $this->name;
    }
}

interface Speaker {
    public function speak(): string;
}

function shout(string $s): string {
    return strtoupper($s);
}
