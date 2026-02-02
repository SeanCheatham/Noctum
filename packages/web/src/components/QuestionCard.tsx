import { useState } from "react";

interface QuestionCardProps {
  questionId: string;
  question: string;
  options?: Array<{ label: string; description: string }>;
  status: "pending" | "answered";
  onRespond: (questionId: string, answer: string) => void;
}

export function QuestionCard({
  questionId,
  question,
  options,
  status,
  onRespond,
}: QuestionCardProps) {
  const [textInput, setTextInput] = useState("");
  const [isSubmitting, setIsSubmitting] = useState(false);

  const handleSubmit = async (answer: string) => {
    setIsSubmitting(true);
    await onRespond(questionId, answer);
    setIsSubmitting(false);
  };

  const handleTextSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (textInput.trim()) {
      handleSubmit(textInput.trim());
    }
  };

  if (status === "answered") {
    return (
      <div className="output-question answered">
        <div className="question-header">Question</div>
        <div className="question-text">{question}</div>
        <div className="question-answered">Answered</div>
      </div>
    );
  }

  return (
    <div className="output-question">
      <div className="question-header">Question</div>
      <div className="question-text">{question}</div>

      {options && options.length > 0 ? (
        <div className="question-options">
          {options.map((option, index) => (
            <button
              key={index}
              className="choice-button"
              onClick={() => handleSubmit(option.label)}
              disabled={isSubmitting}
            >
              <div className="choice-label">{option.label}</div>
              {option.description && <div className="choice-description">{option.description}</div>}
            </button>
          ))}
        </div>
      ) : (
        <form onSubmit={handleTextSubmit} className="question-input">
          <input
            type="text"
            value={textInput}
            onChange={(e) => setTextInput(e.target.value)}
            placeholder="Type your answer..."
            disabled={isSubmitting}
            autoFocus
          />
          <button
            type="submit"
            className="btn btn-primary"
            disabled={isSubmitting || !textInput.trim()}
          >
            {isSubmitting ? "Sending..." : "Submit"}
          </button>
        </form>
      )}
    </div>
  );
}
