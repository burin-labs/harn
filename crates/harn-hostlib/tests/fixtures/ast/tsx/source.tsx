interface ButtonProps {
    label: string;
}

class Button extends Component<ButtonProps> {
    render() {
        return <button>{this.props.label}</button>;
    }
}

function App() {
    return <Button label="hi" />;
}
