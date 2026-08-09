namespace Fixture
{
    public class Greeter
    {
        public string Name { get; set; }

        public string Greet()
        {
            return "hello " + Name;
        }
    }

    public interface ISpeaker
    {
        string Speak();
    }

    public enum Color
    {
        Red,
        Green,
        Blue
    }
}
