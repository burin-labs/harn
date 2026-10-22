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

    public record TagResponse(Guid Id, string Name)
    {
        public TagResponse(Guid id) : this(id, "") { }

        public string Label() => Name.ToUpper();
    }

    public record struct Point(int X, int Y);
}
