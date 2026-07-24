import Foundation

func message(from notification: Notification) -> String {
    notification.userInfo?["message"] as? String ?? "Collaboration error"
}
