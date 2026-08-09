class Header {
    constructor(title) {
        this.title = title;
    }
    render() {
        return <h1>{this.title}</h1>;
    }
}

function shout(text) {
    return text.toUpperCase();
}
