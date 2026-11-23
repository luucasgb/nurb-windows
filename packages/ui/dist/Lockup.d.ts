export default function Lockup({ size, onNavigate, }: {
    size?: "header" | "panel" | "about";
    onNavigate?: () => void;
}): import("react").JSX.Element;
