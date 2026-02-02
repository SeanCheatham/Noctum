import { useState } from "react";

interface PermissionCardProps {
  permissionId: string;
  description: string;
  command?: string;
  status: "pending" | "approved" | "denied";
  onRespond: (permissionId: string, approved: boolean) => void;
}

export function PermissionCard({
  permissionId,
  description,
  command,
  status,
  onRespond,
}: PermissionCardProps) {
  const [isSubmitting, setIsSubmitting] = useState(false);

  const handleRespond = async (approved: boolean) => {
    setIsSubmitting(true);
    await onRespond(permissionId, approved);
    setIsSubmitting(false);
  };

  if (status === "approved") {
    return (
      <div className="output-permission approved">
        <div className="permission-header">Permission Granted</div>
        <div className="permission-description">{description}</div>
        {command && <div className="permission-command">{command}</div>}
      </div>
    );
  }

  if (status === "denied") {
    return (
      <div className="output-permission denied">
        <div className="permission-header">Permission Denied</div>
        <div className="permission-description">{description}</div>
        {command && <div className="permission-command">{command}</div>}
      </div>
    );
  }

  return (
    <div className="output-permission">
      <div className="permission-header">Permission Required</div>
      <div className="permission-description">{description}</div>
      {command && <div className="permission-command">{command}</div>}

      <div className="permission-actions">
        <button
          className="btn btn-approve"
          onClick={() => handleRespond(true)}
          disabled={isSubmitting}
        >
          {isSubmitting ? "..." : "Approve"}
        </button>
        <button
          className="btn btn-deny"
          onClick={() => handleRespond(false)}
          disabled={isSubmitting}
        >
          {isSubmitting ? "..." : "Deny"}
        </button>
      </div>
    </div>
  );
}
