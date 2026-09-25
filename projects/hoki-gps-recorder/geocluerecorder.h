// SPDX-License-Identifier: GPL-3.0-only
#pragma once
#include <QObject>
#include <QFile>
#include <QTimer>
#include <QJsonObject>
#include <QHash>
#include <QDBusConnection>
#include <QDBusMessage>
#include <memory>

// A fresh receiver per session prevents queued signals from a stopped session
// being delivered into a immediately restarted recording.
class RecorderSignalReceiver : public QObject {
    Q_OBJECT
public:
    using QObject::QObject;
public slots:
    void receive(const QDBusMessage &message) { emit received(message); }
signals:
    void received(const QDBusMessage &message);
};

class GeoClueRecorder : public QObject {
    Q_OBJECT
    Q_PROPERTY(bool recording READ recording NOTIFY changed)
    Q_PROPERTY(QString status READ status NOTIFY changed)
    Q_PROPERTY(QString detail READ detail NOTIFY changed)
    Q_PROPERTY(QString fileName READ fileName NOTIFY changed)
    Q_PROPERTY(QString error READ error NOTIFY changed)
    Q_PROPERTY(int events READ events NOTIFY changed)
    Q_PROPERTY(int elapsed READ elapsed NOTIFY changed)
    Q_PROPERTY(int visible READ visible NOTIFY changed)
    Q_PROPERTY(int used READ used NOTIFY changed)
    Q_PROPERTY(int detected READ signalCount NOTIFY changed)
    Q_PROPERTY(int fixAge READ fixAge NOTIFY changed)
public:
    explicit GeoClueRecorder(QObject *parent=nullptr);
    ~GeoClueRecorder() override;
    bool recording() const { return m_recording; }
    QString status() const { return m_status; }
    QString detail() const { return m_detail; }
    QString fileName() const { return m_file.fileName().section('/', -1); }
    QString error() const { return m_error; }
    int events() const { return m_events; }
    int elapsed() const;
    int visible() const { return m_visible; }
    int used() const { return m_used; }
    int signalCount() const { return m_signals; }
    int fixAge() const;
    Q_INVOKABLE void start();
    Q_INVOKABLE void stop();
    static QJsonValue encode(const QVariant &value);
    static bool isFreshTimestamp(qint64 timestamp, qint64 utcNow, qint64 elapsedMs);
signals:
    void changed();
private slots:
    void receive(const QDBusMessage &message);
    void ownerChanged(const QString &, const QString &, const QString &);
private:
    void acquire();
    void recover(const QString &reason);
    void request(const QString &iface, const QString &method, const QList<QVariant> &args={}, int continuation=0);
    void recordMessage(const QDBusMessage &, const QString &, const QString &, const QString &, bool applyToUi=true);
    void write(QJsonObject event);
    void finish(const QString &reason);
    void resetFix();
    static qint64 bootMs();
    QFile m_file;
    QTimer m_tick, m_recovery;
    std::unique_ptr<QDBusConnection> m_bus;
    QObject *m_ownerWatcher=nullptr;
    RecorderSignalReceiver *m_receiver=nullptr;
    QHash<QString,quint64> m_signalVersions;
    QString m_busName, m_status="Ready", m_detail="No live fix", m_error;
    bool m_storageFailed=false;
    bool m_recording=false, m_finishing=false, m_recovering=false, m_acquiring=false;
    int m_recoveryAttempts=0;
    int m_events=0, m_visible=0, m_used=0, m_signals=0, m_generation=0;
    qint64 m_startBoot=0, m_endBoot=0, m_nextHeartbeat=0, m_lastFixBoot=-1, m_lastSignalBoot=-1;
};
